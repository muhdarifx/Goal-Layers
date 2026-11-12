//! Typed, read-only access to the two specific local Codex stores needed for old goals.
use diesel::prelude::*;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read},
    path::Path,
};

diesel::table! {
    thread_goals (thread_id) {
        thread_id -> Text,
        objective -> Text,
        status -> Text,
        token_budget -> Nullable<BigInt>,
        tokens_used -> BigInt,
        time_used_seconds -> BigInt,
        created_at_ms -> BigInt,
        updated_at_ms -> BigInt,
    }
}
diesel::table! {
    threads (id) {
        id -> Text,
        title -> Text,
        cwd -> Text,
        first_user_message -> Text,
        project_id -> Nullable<Text>,
        updated_at -> BigInt,
        archived -> Bool,
        rollout_path -> Text,
    }
}

#[derive(Queryable, Selectable)]
#[diesel(table_name=thread_goals)]
struct Goal {
    thread_id: String,
    objective: String,
    status: String,
    token_budget: Option<i64>,
    tokens_used: i64,
    time_used_seconds: i64,
    created_at_ms: i64,
    updated_at_ms: i64,
}
#[derive(Queryable, Selectable)]
#[diesel(table_name=threads)]
struct Thread {
    id: String,
    title: String,
    cwd: String,
    first_user_message: String,
    project_id: Option<String>,
    updated_at: i64,
    archived: bool,
    rollout_path: String,
}

fn repository_url(path: &str) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(file.take(65536)).read_line(&mut line).ok()?;
    let record: Value = serde_json::from_str(&line).ok()?;
    (record["type"] == "session_meta").then_some(())?;
    record["payload"]["git"]["repository_url"]
        .as_str()
        .map(str::to_owned)
}

/// Percent encoding prevents path characters from changing the mandatory read-only URI option.
fn readonly_url(path: &Path) -> String {
    let mut url = String::from("file:");
    for byte in path.to_string_lossy().replace('\\', "/").bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
            url.push(byte as char)
        } else {
            url.push_str(&format!("%{byte:02X}"))
        }
    }
    url.push_str("?mode=ro");
    url
}

/// No migrations or writes: SQLite rejects writes at the connection's URI boundary.
pub fn snapshot(
    home: &Path,
) -> Result<Vec<(Value, Value)>, Box<dyn std::error::Error + Send + Sync>> {
    let mut goals_db = SqliteConnection::establish(&readonly_url(&home.join("goals_1.sqlite")))?;
    let goals = thread_goals::table
        .select(Goal::as_select())
        .load::<Goal>(&mut goals_db)?;
    let mut state_db = SqliteConnection::establish(&readonly_url(&home.join("state_5.sqlite")))?;
    let mut metadata = HashMap::new();
    // Chunking keeps parameter use below SQLite's conservative variable limit.
    let ids: Vec<&str> = goals.iter().map(|g| g.thread_id.as_str()).collect();
    for ids in ids.chunks(500) {
        for row in threads::table
            .filter(threads::id.eq_any(ids))
            .select(Thread::as_select())
            .load::<Thread>(&mut state_db)?
        {
            metadata.insert(row.id.clone(), row);
        }
    }
    Ok(goals.into_iter().map(|g|{
        let thread=match metadata.remove(&g.thread_id) {
            Some(t)=>json!({"id":t.id,"name":t.title,"preview":t.first_user_message,"cwd":t.cwd,"projectId":t.project_id,"updatedAt":t.updated_at,"archived":t.archived,"repositoryUrl":repository_url(&t.rollout_path)}),
            None=>json!({"id":g.thread_id,"name":g.objective.chars().take(100).collect::<String>(),"preview":g.objective,"cwd":"","projectId":null,"updatedAt":g.updated_at_ms/1000,"historyUnavailable":true}),
        };
        let status=match g.status.as_str(){"usage_limited"=>"usageLimited","budget_limited"=>"budgetLimited",s=>s};
        let goal=json!({"threadId":g.thread_id,"objective":g.objective,"status":status,"tokenBudget":g.token_budget,"tokensUsed":g.tokens_used,"timeUsedSeconds":g.time_used_seconds,"createdAt":g.created_at_ms,"updatedAt":g.updated_at_ms,"source":"local-readonly-store"});
        (thread,goal)
    }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovers_goal_created_after_chat_started() {
        use diesel::connection::SimpleConnection;
        let home = std::env::temp_dir().join(format!(
            "atlas-later-goal-{}-{}",
            std::process::id(),
            crate::now()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let rollout = home.join("rollout.jsonl");
        std::fs::write(&rollout, "{\"type\":\"session_meta\",\"payload\":{\"git\":{\"repository_url\":\"https://example.com/repository.git\"}}}\n").unwrap();
        assert_eq!(
            repository_url(rollout.to_str().unwrap()).as_deref(),
            Some("https://example.com/repository.git")
        );
        assert_eq!(repository_url("missing-file"), None);
        let mut goals =
            SqliteConnection::establish(home.join("goals_1.sqlite").to_str().unwrap()).unwrap();
        goals.batch_execute("CREATE TABLE thread_goals (thread_id TEXT PRIMARY KEY, objective TEXT NOT NULL, status TEXT NOT NULL, token_budget BIGINT, tokens_used BIGINT NOT NULL, time_used_seconds BIGINT NOT NULL, created_at_ms BIGINT NOT NULL, updated_at_ms BIGINT NOT NULL);").unwrap();
        let mut state =
            SqliteConnection::establish(home.join("state_5.sqlite").to_str().unwrap()).unwrap();
        state.batch_execute("CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT NOT NULL, cwd TEXT NOT NULL, first_user_message TEXT NOT NULL, project_id TEXT, updated_at BIGINT NOT NULL, archived BOOL NOT NULL, rollout_path TEXT NOT NULL); INSERT INTO threads VALUES ('later', 'Earlier discussion', '', 'Unrelated opening message', 'project', 1, 0, '');").unwrap();
        assert!(snapshot(&home).unwrap().is_empty());
        goals.batch_execute("INSERT INTO thread_goals VALUES ('later', 'Ship the new feature', 'active', NULL, 0, 0, 2000, 2000);").unwrap();
        let rows = snapshot(&home).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0["projectId"], "project");
        assert_eq!(rows[0].1["objective"], "Ship the new feature");
        assert_eq!(rows[0].1["threadId"], "later");
        drop(goals);
        drop(state);
        std::fs::remove_dir_all(home).unwrap();
    }
    diesel::table! { __diesel_schema_migrations(version) {version -> Text,} }
    #[test]
    fn readonly_connection_rejects_writes_to_a_temporary_database() {
        use diesel::migration::MigrationConnection;
        let path = std::env::temp_dir().join(format!(
            "atlas-readonly-{}-{}.sqlite",
            std::process::id(),
            crate::now()
        ));
        let mut writable = SqliteConnection::establish(path.to_str().unwrap()).unwrap();
        writable.setup().unwrap();
        drop(writable);
        let mut readonly = SqliteConnection::establish(&readonly_url(&path)).unwrap();
        let result = diesel::insert_into(__diesel_schema_migrations::table)
            .values(__diesel_schema_migrations::version.eq("test"))
            .execute(&mut readonly);
        assert!(
            result
                .unwrap_err()
                .to_string()
                .to_lowercase()
                .contains("readonly")
        );
        drop(readonly);
        let mut writable = SqliteConnection::establish(path.to_str().unwrap()).unwrap();
        assert_eq!(
            __diesel_schema_migrations::table
                .count()
                .get_result::<i64>(&mut writable)
                .unwrap(),
            0
        );
        drop(writable);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn path_cannot_override_readonly_and_missing_store_is_not_created() {
        assert_eq!(
            readonly_url(Path::new("C:/a b/#c?mode=rw.db")),
            "file:C:/a%20b/%23c%3Fmode%3Drw.db?mode=ro"
        );
        let path = std::env::temp_dir().join(format!(
            "atlas-missing-{}-{}.sqlite",
            std::process::id(),
            crate::now()
        ));
        assert!(SqliteConnection::establish(&readonly_url(&path)).is_err());
        assert!(!path.exists());
    }
}
