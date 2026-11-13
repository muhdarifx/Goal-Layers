use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Sse, sse::Event},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    convert::Infallible,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{Mutex, RwLock, broadcast},
};
use tokio_stream::{StreamExt, wrappers::BroadcastStream};
mod account;
mod chat_actions;
mod digest;
mod flow;
mod fork_review;
mod history;
mod live_status;
mod scope_breakdown;
mod status_updates;
mod trace;

type Error = Box<dyn std::error::Error + Send + Sync>;
type ApiError = (StatusCode, String);

/// Prefer the desktop-bundled Codex; an older PATH install can omit paginated chats.
fn codex_executable() -> std::ffi::OsString {
    if let Some(path) = std::env::var_os("GOAL_ATLAS_CODEX") {
        return path;
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let root = PathBuf::from(local).join("OpenAI/Codex/bin");
        if let Ok(entries) = std::fs::read_dir(root) {
            let latest = entries
                .flatten()
                .map(|e| e.path().join("codex.exe"))
                .filter(|p| p.is_file())
                .max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok());
            if let Some(path) = latest {
                return path.into_os_string();
            }
        }
    }
    "codex".into()
}

/// Owns an observer and explicit message-queue client; dropping it terminates only our child server.
struct Codex {
    _child: Child,
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
    sequence: u64,
}
impl Codex {
    /// Respect project pagination instead of assuming a default page is the whole workspace.
    async fn projects(&mut self) -> Result<Vec<Value>, Error> {
        let mut projects = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen = HashSet::new();
        loop {
            let result = self
                .call("project/list", json!({"limit":1000,"cursor":cursor}))
                .await?;
            projects.extend(
                result["data"]
                    .as_array()
                    .ok_or("No project list")?
                    .iter()
                    .cloned(),
            );
            match result["nextCursor"].as_str() {
                Some(next) if seen.insert(next.to_owned()) => cursor = Some(next.to_owned()),
                Some(_) => return Err("Repeated project cursor".into()),
                None => {
                    return Ok(tokio::task::spawn_blocking(move || {
                        for project in &mut projects {
                            add_worktree_roots(project);
                        }
                        projects
                    })
                    .await?);
                }
            }
        }
    }
    async fn connect() -> Result<Self, Error> {
        let executable = codex_executable();
        let mut child = Command::new(executable)
            .arg("app-server")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let input = child.stdin.take().ok_or("Missing Codex input")?;
        let output = BufReader::new(child.stdout.take().ok_or("Missing Codex output")?).lines();
        let mut client = Self {
            _child: child,
            input,
            output,
            sequence: 0,
        };
        client.call("initialize", json!({"clientInfo":{"name":"goal_atlas","version":"0.1.0"},"capabilities":{"experimentalApi":true}})).await?;
        client
            .input
            .write_all(b"{\"method\":\"initialized\"}\n")
            .await?;
        Ok(client)
    }
    /// Mutations are explicit; review cancellation interrupts only its owned fork.
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, Error> {
        if !matches!(
            method,
            "initialize"
                | "account/read"
                | "account/rateLimits/read"
                | "project/list"
                | "thread/list"
                | "thread/goal/get"
                | "thread/items/list"
                | "thread/read"
                | "thread/queue/add"
                | "thread/queue/list"
                | "thread/resume"
                | "thread/fork"
                | "thread/goal/clear"
                | "thread/delete"
                | "turn/start"
                | "turn/interrupt"
                | "config/read"
                | "configRequirements/read"
                | "model/list"
        ) {
            return Err("Codex client refused method".into());
        }
        self.sequence += 1;
        let id = self.sequence;
        let request = format!("{}\n", json!({"id":id,"method":method,"params":params}));
        self.input.write_all(request.as_bytes()).await?;
        tokio::time::timeout(Duration::from_secs(45), async {
            while let Some(line) = self.output.next_line().await? {
                let value: Value = serde_json::from_str(&line)?;
                if value["id"].as_u64() == Some(id) {
                    if !value["error"].is_null() { return Err(format!("Codex {method}: {}", value["error"]).into()); }
                    return Ok(value["result"].clone());
                }
                // Our connection cannot approve execution or grant permissions.
                if value.get("id").is_some() && value.get("method").is_some() {
                    let rejection = format!("{}\n", json!({"id":value["id"],"error":{"code":-32601,"message":"Read-only observer"}}));
                    self.input.write_all(rejection.as_bytes()).await?;
                }
            }
            Err::<Value, Error>("Codex connection closed".into())
        }).await.map_err(|_| format!("Codex {method} timed out"))?
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
struct Node {
    id: String,
    thread_id: String,
    parent_id: Option<String>,
    title: String,
    status: String,
    lane: String,
    evidence: String,
    next: String,
    #[serde(default)]
    acceptance: String,
    #[serde(default)]
    blockers: Vec<String>,
    #[serde(default)]
    reported_evidence: String,
    #[serde(default)]
    status_report_id: Option<String>,
    #[serde(default)]
    hidden: bool,
    revision: u64,
}
#[derive(Clone, Default, Serialize)]
struct Catalog {
    projects: Vec<Value>,
    threads: BTreeMap<String, Value>,
    goals: BTreeMap<String, Value>,
    scanning: bool,
    scanned: usize,
    skipped: usize,
    history_count: Option<usize>,
    history_error: Option<String>,
    updated: u64,
    error: Option<String>,
}
struct App {
    trace: Arc<std::sync::Mutex<trace::Cache>>,
    flow: flow::Hub,
    codex: Mutex<Codex>,
    catalog: RwLock<Catalog>,
    nodes: Mutex<Vec<Node>>,
    message_receipts: Mutex<BTreeMap<String, Value>>,
    reports: RwLock<BTreeMap<String, Vec<Node>>>,
    codex_home: PathBuf,
    file: PathBuf,
    updates: broadcast::Sender<u64>,
}

/// Epoch milliseconds are display metadata, not ordering authority for node writes.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}

/// Older chats use a working directory rather than an explicit project ID.
/// Match the longest configured root on a path boundary; ambiguous roots stay unassigned.
fn add_worktree_roots(project: &mut Value) {
    let mut aliases = Vec::new();
    let mut repositories = Vec::new();
    for root in project["roots"].as_array().into_iter().flatten() {
        if let Ok(config) =
            std::fs::read_to_string(PathBuf::from(text(root, "path")).join(".git/config"))
        {
            let mut origin = false;
            for line in config.lines().map(str::trim) {
                if line.starts_with('[') {
                    origin = line == "[remote \"origin\"]";
                }
                if origin {
                    if let Some(("url", url)) =
                        line.split_once('=').map(|(k, v)| (k.trim(), v.trim()))
                    {
                        repositories.push(url.to_owned());
                    }
                }
            }
        }
        let directory = PathBuf::from(text(root, "path")).join(".git/worktrees");
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(gitdir) = std::fs::read_to_string(entry.path().join("gitdir")) else {
                continue;
            };
            let gitdir = PathBuf::from(gitdir.trim());
            if gitdir.is_absolute() && gitdir.file_name().is_some_and(|name| name == ".git") {
                if let Some(parent) = gitdir.parent() {
                    aliases.push(json!({"path":parent.to_string_lossy()}));
                }
            }
        }
    }
    project["worktreeRoots"] = json!(aliases);
    project["repositoryUrls"] = json!(repositories);
}

fn project_for(thread: &Value, projects: &[Value]) -> Option<String> {
    if let Some(id) = thread["projectId"].as_str().filter(|id| !id.is_empty()) {
        return Some(id.to_owned());
    }
    fn normalize(path: &str) -> String {
        let p = path.replace('\\', "/").trim_end_matches('/').to_owned();
        // Native Windows metadata uses extended-length paths; project roots often do not.
        let p = p.strip_prefix("//?/").unwrap_or(&p).to_owned();
        // Migrated chats retain /mnt/e or C:/mnt/e while project roots use E:/.
        let mount = p.strip_prefix("/mnt/").or_else(|| {
            (p.as_bytes().get(1) == Some(&b':'))
                .then(|| p.get(2..).and_then(|s| s.strip_prefix("/mnt/")))
                .flatten()
        });
        if let Some(mount) = mount {
            let bytes = mount.as_bytes();
            if bytes.first().is_some_and(u8::is_ascii_alphabetic)
                && (bytes.len() == 1 || bytes.get(1) == Some(&b'/'))
            {
                return format!("{}:{}", &mount[..1], &mount[1..]).to_lowercase();
            }
        }
        if p.as_bytes().get(1) == Some(&b':') {
            p.to_lowercase()
        } else {
            p
        }
    }
    let cwd = normalize(text(thread, "cwd"));
    let mut candidates = Vec::new();
    for project in projects {
        for root in project["roots"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(project["worktreeRoots"].as_array().into_iter().flatten())
        {
            let path = normalize(text(root, "path"));
            if !path.is_empty() && (cwd == path || cwd.starts_with(&(path.clone() + "/"))) {
                candidates.push((path.len(), text(project, "id").to_owned()));
            }
        }
    }
    candidates.sort();
    if candidates.is_empty() {
        let url = text(thread, "repositoryUrl");
        if !url.is_empty() {
            let matches: Vec<_> = projects
                .iter()
                .filter(|p| {
                    p["repositoryUrls"]
                        .as_array()
                        .is_some_and(|urls| urls.iter().any(|value| value.as_str() == Some(url)))
                })
                .collect();
            return (matches.len() == 1).then(|| text(matches[0], "id").to_owned());
        }
    }
    let best = candidates.last()?;
    if candidates.iter().any(|c| c.0 == best.0 && c.1 != best.1) {
        None
    } else {
        Some(best.1.clone())
    }
}

/// Refresh saved goal records independently of the API's archived-thread limitation.
async fn merge_history(app: &App) {
    let home = app.codex_home.clone();
    let result = tokio::task::spawn_blocking(move || history::snapshot(&home)).await;
    let mut data = app.catalog.write().await;
    match result {
        Ok(Ok(rows)) => {
            data.goals
                .retain(|_, g| text(g, "source") != "local-readonly-store");
            data.history_count = Some(rows.len());
            data.history_error = None;
            for (thread, goal) in rows {
                let id = text(&goal, "threadId").to_owned();
                data.threads.insert(id.clone(), thread);
                data.goals.insert(id, goal);
            }
        }
        Ok(Err(error)) => data.history_error = Some(error.to_string()),
        Err(error) => data.history_error = Some(error.to_string()),
    }
}

/// Refresh one bounded page. Historical scans and live scans share this loader.
async fn page(
    app: &App,
    cursor: Option<&str>,
    archived: bool,
    limit: u32,
) -> Result<Option<String>, Error> {
    let mut client = app.codex.lock().await;
    let result = client.call("thread/list", json!({"limit":limit,"cursor":cursor,"archived":archived,"useStateDbOnly":true,"modelProviders":[],"sortKey":"updated_at","sourceKinds":["cli","vscode","exec","appServer","subAgent","subAgentReview","subAgentCompact","subAgentThreadSpawn","subAgentOther","unknown"]})).await?;
    let rows = result["data"]
        .as_array()
        .ok_or("Codex returned no thread page")?;
    for thread in rows {
        let id = text(thread, "id");
        let saved = {
            let data = app.catalog.read().await;
            data.history_count
                .map(|_| json!({"goal":data.goals.get(id)}))
        };
        let goal = match saved {
            Some(goal) => goal,
            None => match client.call("thread/goal/get", json!({"threadId":id})).await {
                Ok(goal) => goal,
                Err(error) if error.to_string().contains("thread not found") => {
                    // Deleted rollout entries can remain in the index; one must not block the import.
                    let mut data = app.catalog.write().await;
                    data.skipped += 1;
                    data.scanned += 1;
                    continue;
                }
                Err(error) => return Err(error),
            },
        };
        let mut data = app.catalog.write().await;
        let mut indexed = thread.clone();
        indexed["archived"] = json!(archived);
        // The API can omit an archived goal's saved project assignment; do not erase it.
        if data
            .goals
            .get(id)
            .is_none_or(|g| text(g, "source") != "local-readonly-store")
        {
            data.threads.insert(id.to_owned(), indexed);
        }
        if goal["goal"].is_object() {
            data.goals.insert(id.to_owned(), goal["goal"].clone());
        } else {
            data.goals.remove(id);
        }
        data.scanned += 1;
    }
    {
        let mut data = app.catalog.write().await;
        data.updated = now();
        data.error = None;
    }
    let _ = app.updates.send(now());
    Ok(result["nextCursor"].as_str().map(str::to_owned))
}

/// Reconnect after errors; retain last known data with an explicit stale/error state.
async fn synchronize(app: Arc<App>) {
    loop {
        let result: Result<(), Error> = async {
            merge_history(&app).await;
            let projects = app.codex.lock().await.projects().await?;
            app.catalog.write().await.projects = projects;
            app.catalog.write().await.scanning = true;
            for archived in [false, true] {
                let mut cursor = None;
                let mut seen = HashSet::new();
                loop {
                    let next = page(&app, cursor.as_deref(), archived, 100).await?;
                    match next {
                        Some(value) if seen.insert(value.clone()) => cursor = Some(value),
                        Some(_) => return Err("Repeated history cursor".into()),
                        None => break,
                    }
                    tokio::task::yield_now().await;
                }
            }
            app.catalog.write().await.scanning = false;
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                merge_history(&app).await;
                page(&app, None, false, 30).await?;
                // Old goals can change without a new message in their chat.
                let ids: Vec<String> = app
                    .catalog
                    .read()
                    .await
                    .goals
                    .iter()
                    .filter(|(_, g)| text(g, "source") != "local-readonly-store")
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in ids {
                    let goal = app
                        .codex
                        .lock()
                        .await
                        .call("thread/goal/get", json!({"threadId":id}))
                        .await?;
                    let mut data = app.catalog.write().await;
                    if goal["goal"].is_object() {
                        data.goals.insert(id, goal["goal"].clone());
                    } else {
                        data.goals.remove(&id);
                    }
                }
                let projects = app.codex.lock().await.projects().await?;
                app.catalog.write().await.projects = projects;
                let _ = app.updates.send(now());
            }
        }
        .await;
        if let Err(error) = result {
            app.catalog.write().await.error = Some(error.to_string());
            let _ = app.updates.send(now());
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
        match Codex::connect().await {
            Ok(client) => *app.codex.lock().await = client,
            Err(error) => app.catalog.write().await.error = Some(error.to_string()),
        }
    }
}

/// Send only UI-needed fields; omit raw tool arguments, credentials and command output.
async fn catalog(State(app): State<Arc<App>>) -> Json<Value> {
    let data = app.catalog.read().await;
    let threads: Vec<Value> = data.threads.values().map(|t| json!({"id":t["id"],"name":t["name"],"preview":t["preview"],"projectId":project_for(t,&data.projects),"cwd":t["cwd"],"updatedAt":t["updatedAt"],"archived":t["archived"],"parentThreadId":t["parentThreadId"]})).collect();
    Json(
        json!({"projects":data.projects,"threads":threads,"goals":data.goals,"scanning":data.scanning,"scanned":data.scanned,"skipped":data.skipped,"historyCount":data.history_count,"historyError":data.history_error,"updated":data.updated,"error":data.error,"nodes":*app.nodes.lock().await}),
    )
}

/// Retrieve recent recorded activity on demand, with bounded history and truthful statuses.
async fn detail(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !app.catalog.read().await.threads.contains_key(&id) {
        return Err((StatusCode::NOT_FOUND, "Unknown chat".into()));
    }
    let result = app
        .codex
        .lock()
        .await
        .call(
            "thread/items/list",
            json!({"threadId":id,"sortDirection":"desc","limit":100}),
        )
        .await
        .unwrap_or_else(|error| json!({"data":[],"historyError":error.to_string()}));
    let rows = result["data"]
        .as_array()
        .ok_or_else(|| api_error("No item history"))?;
    fork_review::resume_pending(&app, &id).await;
    let mut review_rows = rows.clone();
    review_rows.extend(fork_review::saved_rows(&app, &id).await);
    let mut status_updates = status_updates::apply(&app, &id, &review_rows).await?;
    status_updates.extend(scope_breakdown::apply(&app, &id, &review_rows).await?);
    let reviews = fork_review::snapshots(&app, &id).await;
    status_updates.retain(|status| {
        !reviews
            .iter()
            .any(|review| review["request_id"] == status["request_id"])
    });
    status_updates.extend(reviews);
    live_status::refresh(&app, &id).await?;
    let steps = reported_steps(rows, &id);
    app.reports.write().await.insert(id.clone(), steps.clone());
    let original_page = app
        .codex
        .lock()
        .await
        .call(
            "thread/items/list",
            json!({"threadId":id,"sortDirection":"asc","limit":20}),
        )
        .await
        .unwrap_or_else(|error| json!({"data":[],"historyError":error.to_string()}));
    let original = original_page["data"].as_array().and_then(|rows| {
        rows.iter().find_map(|entry| {
            let item = entry.get("item").unwrap_or(entry);
            (text(item, "type") == "userMessage").then(|| {
                item["content"]
                    .as_array()
                    .map(|content| {
                        content
                            .iter()
                            .filter_map(|part| part["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default()
            })
        })
    });
    let items: Vec<Value> = rows.iter().filter_map(|item| {
        // Some versions wrap each item with its position; accept either documented shape.
        let item = item.get("item").unwrap_or(item);
        let kind = text(item,"type");
        if matches!(kind,"reasoning"|"hookPrompt"|"userMessage") { return None; }
        let title = match kind { "agentMessage" => text(item,"text").chars().take(4000).collect::<String>(), "commandExecution" => "Command execution".into(), "fileChange" => "File change".into(), "mcpToolCall" => format!("{} / {}",text(item,"server"),text(item,"tool")), "webSearch" => "Web research".into(), _ => kind.to_owned() };
        Some(json!({"id":item["id"],"kind":kind,"title":title,"status":item.get("status").cloned().unwrap_or(json!("recorded"))}))
    }).collect();
    Ok(Json(
        json!({"items":items,"statusUpdates":status_updates,"updatedNodes":app.nodes.lock().await.iter().filter(|n|n.thread_id==id).cloned().collect::<Vec<_>>(),"steps":steps,"original":original,"historyError":result["historyError"],"more":!result["nextCursor"].is_null()}),
    ))
}
async fn scope_trace(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let thread = app
        .catalog
        .read()
        .await
        .threads
        .get(&id)
        .cloned()
        .ok_or((StatusCode::NOT_FOUND, "Unknown chat".into()))?;
    let mut path = thread["path"].as_str().map(PathBuf::from);
    if path.is_none() {
        let result = app
            .codex
            .lock()
            .await
            .call("thread/read", json!({"threadId":id,"includeTurns":false}))
            .await
            .map_err(api_error)?;
        path = result["thread"]["path"].as_str().map(PathBuf::from);
    }
    let Some(path) = path else {
        return Ok(Json(
            json!({"available":false,"reason":"Session log unavailable"}),
        ));
    };
    let mut graph = app
        .flow
        .documents
        .lock()
        .await
        .get(&id)
        .map(|d| d.graph.clone());
    if let Some(graph) = &mut graph {
        for n in app.nodes.lock().await.iter().filter(|n| n.thread_id == id) {
            if let Some(source) = graph.nodes.iter_mut().find(|s| s.id == n.id) {
                source.title = n.title.clone();
            } else {
                graph.nodes.push(flow::Component {
                    id: n.id.clone(),
                    title: n.title.clone(),
                    kind: "Manual step".into(),
                    lane: n.lane.clone(),
                    status: n.status.clone(),
                    description: n.evidence.clone(),
                    next: n.next.clone(),
                    evidence: vec![],
                });
            }
        }
    }
    let cache = app.trace.clone();
    let criteria = app
        .nodes
        .lock()
        .await
        .iter()
        .filter(|n| n.thread_id == id && !n.acceptance.is_empty())
        .map(|n| (n.id.clone(), n.acceptance.clone()))
        .collect();
    let result = tokio::task::spawn_blocking(move || {
        cache
            .lock()
            .map_err(|_| "Trace cache unavailable".to_owned())?
            .read(&path, &id, graph.as_ref(), &criteria)
    })
    .await
    .map_err(api_error)?;
    Ok(Json(result.unwrap_or_else(
        |reason| json!({"available":false,"reason":reason}),
    )))
}

/// Import the newest explicit scope table; activity never implies completion.
fn reported_steps(rows: &[Value], thread_id: &str) -> Vec<Node> {
    for entry in rows {
        let item = entry.get("item").unwrap_or(entry);
        if text(item, "type") != "agentMessage" {
            continue;
        }
        let Some((_, table)) = text(item, "text").split_once("Scope check:") else {
            continue;
        };
        let mut nodes = Vec::new();
        for line in table
            .lines()
            .skip_while(|line| !line.trim().starts_with('|'))
        {
            if !line.trim().starts_with('|') {
                break;
            }
            let cells: Vec<&str> = line
                .trim()
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect();
            if cells.len() < 4
                || !cells[0].starts_with('R')
                || !cells[0].chars().nth(1).is_some_and(|c| c.is_ascii_digit())
            {
                continue;
            }
            let status = match cells[1] {
                "DONE" if !cells[2].is_empty() => "complete",
                "ON TRACK" => "in_progress",
                "BLOCKED" => "blocked",
                _ => "pending",
            };
            nodes.push(Node {
                id: format!(
                    "report-{}",
                    cells[0].split_whitespace().next().unwrap_or("step")
                ),
                thread_id: thread_id.into(),
                parent_id: None,
                title: cells[0].into(),
                status: status.into(),
                lane: if cells[1] == "DRIFT" {
                    "drift"
                } else {
                    "aligned"
                }
                .into(),
                evidence: format!(
                    "Reported in {} (not independently verified).\n{}",
                    text(item, "id"),
                    cells[2]
                ),
                next: cells[3].into(),
                revision: 0,
                acceptance: String::new(),
                blockers: Vec::new(),
                reported_evidence: String::new(),
                status_report_id: None,
                hidden: false,
            });
        }
        if !nodes.is_empty() {
            return nodes;
        }
    }
    Vec::new()
}

fn api_error(error: impl std::fmt::Display) -> ApiError {
    (StatusCode::BAD_GATEWAY, error.to_string())
}

/// Validate the complete tree before atomic persistence; revisions reject stale edits.
fn validate(nodes: &[Node], candidate: &Node) -> Result<(), String> {
    if candidate.id == "original-goal" && candidate.hidden {
        return Err("The original goal stays visible".into());
    }
    if candidate.id.is_empty()
        || candidate.id.len() > 100
        || candidate.title.trim().is_empty()
        || candidate.title.len() > 2000
        || candidate.evidence.len() > 12000
        || candidate.next.len() > 4000
        || candidate.acceptance.len() > 4000
        || candidate.reported_evidence.len() > 12000
        || candidate.blockers.len() > 20
        || candidate.blockers.iter().any(|s| s.len() > 1000)
        || candidate
            .status_report_id
            .as_ref()
            .is_some_and(|s| s.len() > 100)
    {
        return Err("Invalid or oversized node".into());
    }
    if !matches!(
        candidate.status.as_str(),
        "unknown" | "pending" | "in_progress" | "blocked" | "complete"
    ) || !matches!(candidate.lane.as_str(), "aligned" | "addition" | "drift")
    {
        return Err("Invalid status or lane".into());
    }
    if candidate.status == "complete"
        && candidate.evidence.trim().is_empty()
        && candidate.reported_evidence.trim().is_empty()
    {
        return Err("Completion needs evidence".into());
    }
    if let Some(old) = nodes
        .iter()
        .find(|n| n.id == candidate.id && n.thread_id == candidate.thread_id)
    {
        if old.revision != candidate.revision {
            return Err("This node changed. Reopen it before saving.".into());
        }
    }
    let mut parent = candidate.parent_id.as_deref();
    let mut seen = HashSet::new();
    seen.insert(candidate.id.as_str());
    while let Some(id) = parent {
        if !seen.insert(id) || seen.len() > 64 {
            return Err("Tree cycle or depth limit".into());
        }
        let node = nodes
            .iter()
            .find(|n| n.id == id && n.thread_id == candidate.thread_id)
            .ok_or("Parent does not belong to this goal")?;
        parent = node.parent_id.as_deref();
    }
    if nodes.len() >= 10000
        && !nodes
            .iter()
            .any(|n| n.id == candidate.id && n.thread_id == candidate.thread_id)
    {
        return Err("Node capacity reached".into());
    }
    Ok(())
}

fn validate_edit(nodes: &[Node], candidate: &Node) -> Result<(), String> {
    if candidate.acceptance.len() > 4000 {
        return Err("Acceptance check is too long".into());
    }
    let existing = nodes
        .iter()
        .find(|n| n.id == candidate.id && n.thread_id == candidate.thread_id)
        .ok_or("Manual step creation is unavailable")?;
    if existing.parent_id != candidate.parent_id {
        return Err("Manual edits cannot change the step hierarchy".into());
    }
    validate(nodes, candidate)
}

/// Manual edits cannot create steps; agent reports retain the ingestion path.
async fn edit_node(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(node): Json<Node>,
) -> Result<Json<Node>, ApiError> {
    persist_node(app, headers, node, false).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VisibilityRequest {
    thread_id: String,
    node_id: String,
    revision: u64,
    hidden: bool,
    confirmed: bool,
}

async fn node_visibility(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(request): Json<VisibilityRequest>,
) -> Result<Json<Node>, ApiError> {
    if request.hidden && !request.confirmed {
        return Err((
            StatusCode::BAD_REQUEST,
            "Confirm hiding this node first".into(),
        ));
    }
    if request.node_id == "original-goal" {
        return Err((
            StatusCode::BAD_REQUEST,
            "The original goal stays visible".into(),
        ));
    }
    let mut known = app.nodes.lock().await.clone();
    if let Some(document) = app.flow.documents.lock().await.get(&request.thread_id) {
        known.extend(document.nodes());
    }
    if let Some(reports) = app.reports.read().await.get(&request.thread_id) {
        known.extend(reports.clone());
    }
    let mut node = known
        .into_iter()
        .find(|n| n.id == request.node_id && n.thread_id == request.thread_id)
        .ok_or((StatusCode::NOT_FOUND, "Node not found".into()))?;
    if node.revision != request.revision {
        return Err((
            StatusCode::CONFLICT,
            "This node changed. Reopen it before saving.".into(),
        ));
    }
    node.hidden = request.hidden;
    persist_node(app, headers, node, false).await
}

async fn bulk_node_visibility(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(requests): Json<Vec<VisibilityRequest>>,
) -> Result<Json<Vec<Node>>, ApiError> {
    if headers.get("x-goal-atlas").and_then(|v| v.to_str().ok()) != Some("1") {
        return Err((StatusCode::FORBIDDEN, "Missing local client header".into()));
    }
    if requests.is_empty() || requests.len() > 500 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Select between 1 and 500 nodes".into(),
        ));
    }
    let thread = &requests[0].thread_id;
    let mut ids = HashSet::new();
    if requests.iter().any(|r| {
        r.thread_id != *thread
            || r.node_id == "original-goal"
            || !ids.insert(&r.node_id)
            || (r.hidden && !r.confirmed)
    }) {
        return Err((
            StatusCode::BAD_REQUEST,
            "Confirm distinct nodes from one goal; the original goal stays visible".into(),
        ));
    }
    if !app.catalog.read().await.threads.contains_key(thread) {
        return Err((StatusCode::NOT_FOUND, "Unknown chat".into()));
    }
    let mut stored = app.nodes.lock().await;
    let mut known = stored.clone();
    if let Some(document) = app.flow.documents.lock().await.get(thread) {
        known.extend(document.nodes());
    }
    if let Some(reports) = app.reports.read().await.get(thread) {
        known.extend(reports.clone());
    }
    let mut changed = Vec::new();
    for r in &requests {
        let mut node = known
            .iter()
            .find(|n| n.id == r.node_id && n.thread_id == *thread)
            .cloned()
            .ok_or((StatusCode::NOT_FOUND, "Node not found".into()))?;
        if node.revision != r.revision {
            return Err((
                StatusCode::CONFLICT,
                "A selected node changed. Select again before hiding.".into(),
            ));
        }
        node.hidden = r.hidden;
        validate_edit(&known, &node).map_err(|e| (StatusCode::CONFLICT, e))?;
        node.revision = node
            .revision
            .checked_add(1)
            .ok_or((StatusCode::CONFLICT, "Revision exhausted".into()))?;
        changed.push(node);
    }
    let mut next = stored.clone();
    for node in &changed {
        let mut parent = node.parent_id.clone();
        while let Some(id) = parent {
            let ancestor = known
                .iter()
                .find(|n| n.id == id && n.thread_id == *thread)
                .ok_or((StatusCode::CONFLICT, "Parent unavailable".into()))?;
            if !next.iter().any(|n| n.id == id && n.thread_id == *thread) {
                next.push(ancestor.clone());
            }
            parent = ancestor.parent_id.clone();
        }
        next.retain(|n| n.id != node.id || n.thread_id != *thread);
        next.push(node.clone());
    }
    let temp = app.file.with_extension("tmp");
    let mut file = tokio::fs::File::create(&temp).await.map_err(api_error)?;
    file.write_all(&serde_json::to_vec_pretty(&next).map_err(api_error)?)
        .await
        .map_err(api_error)?;
    file.sync_all().await.map_err(api_error)?;
    drop(file);
    tokio::fs::rename(temp, &app.file)
        .await
        .map_err(api_error)?;
    *stored = next;
    let _ = app.updates.send(now());
    Ok(Json(changed))
}

async fn report(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(node): Json<Node>,
) -> Result<Json<Node>, ApiError> {
    persist_node(app, headers, node, true).await
}

async fn persist_node(
    app: Arc<App>,
    headers: HeaderMap,
    mut node: Node,
    agent_report: bool,
) -> Result<Json<Node>, ApiError> {
    if headers.get("x-goal-atlas").and_then(|v| v.to_str().ok()) != Some("1") {
        return Err((StatusCode::FORBIDDEN, "Missing local client header".into()));
    }
    if !app
        .catalog
        .read()
        .await
        .threads
        .contains_key(&node.thread_id)
    {
        return Err((StatusCode::NOT_FOUND, "Unknown chat".into()));
    }
    let mut nodes = app.nodes.lock().await;
    let reports = app.reports.read().await;
    let mut known = nodes.clone();
    if let Some(document) = app.flow.documents.lock().await.get(&node.thread_id) {
        for imported in document.nodes() {
            if !known
                .iter()
                .any(|n| n.id == imported.id && n.thread_id == imported.thread_id)
            {
                known.push(imported);
            }
        }
    }
    for imported in reports.get(&node.thread_id).into_iter().flatten() {
        if !known
            .iter()
            .any(|n| n.id == imported.id && n.thread_id == imported.thread_id)
        {
            known.push(imported.clone());
        }
    }
    if agent_report {
        node.hidden = known
            .iter()
            .find(|n| n.id == node.id && n.thread_id == node.thread_id)
            .is_some_and(|n| n.hidden);
        validate(&known, &node)
    } else {
        validate_edit(&known, &node)
    }
    .map_err(|e| (StatusCode::CONFLICT, e))?;
    node.revision = node
        .revision
        .checked_add(1)
        .ok_or((StatusCode::CONFLICT, "Revision exhausted".into()))?;
    if !agent_report {
        node.status_report_id = Some(format!("manual-{}", now()));
    }
    let mut next = nodes.clone();
    // Snapshot imported ancestors so a saved substep remains valid after restart.
    let mut parent = node.parent_id.clone();
    while let Some(id) = parent {
        let ancestor = known
            .iter()
            .find(|n| n.id == id && n.thread_id == node.thread_id)
            .ok_or((StatusCode::CONFLICT, "Parent unavailable".into()))?;
        if !next
            .iter()
            .any(|n| n.id == id && n.thread_id == node.thread_id)
        {
            next.push(ancestor.clone());
        }
        parent = ancestor.parent_id.clone();
    }
    next.retain(|n| n.id != node.id || n.thread_id != node.thread_id);
    next.push(node.clone());
    let bytes = serde_json::to_vec_pretty(&next).map_err(api_error)?;
    let temp = app.file.with_extension("tmp");
    let mut file = tokio::fs::File::create(&temp).await.map_err(api_error)?;
    file.write_all(&bytes).await.map_err(api_error)?;
    file.sync_all().await.map_err(api_error)?;
    drop(file);
    tokio::fs::rename(&temp, &app.file)
        .await
        .map_err(api_error)?;
    *nodes = next;
    let _ = app.updates.send(now());
    Ok(Json(node))
}

/// DNS rebinding and cross-site requests must not expose local conversation content.
async fn local_only(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let host = request
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let allowed = host == "127.0.0.1:4317" || host == "localhost:4317";
    let origin = request
        .headers()
        .get("origin")
        .and_then(|h| h.to_str().ok());
    if !allowed || origin.is_some_and(|o| o != format!("http://{host}")) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    response.headers_mut().insert("content-security-policy", "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'".parse().unwrap());
    response
}

/// Local web entrypoint; observations and explicit queued messages share the local Codex connection.
#[tokio::main]
async fn main() -> Result<(), Error> {
    // Agent integrations publish structured reports to the running app, not to Codex's storage.
    if std::env::args().nth(1).as_deref() == Some("report") {
        let mut bytes = Vec::new();
        tokio::io::stdin()
            .take(32769)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() > 32768 {
            return Err("Report exceeds 32 KiB".into());
        }
        let _: Node = serde_json::from_slice(&bytes)?;
        let mut socket = tokio::net::TcpStream::connect("127.0.0.1:4317").await?;
        let header = format!(
            "POST /api/reports HTTP/1.1\r\nHost: 127.0.0.1:4317\r\nContent-Type: application/json\r\nX-Goal-Atlas: 1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            bytes.len()
        );
        socket.write_all(header.as_bytes()).await?;
        socket.write_all(&bytes).await?;
        let mut response = String::new();
        tokio::time::timeout(
            Duration::from_secs(20),
            socket.take(65536).read_to_string(&mut response),
        )
        .await??;
        if !response.starts_with("HTTP/1.1 200 ") {
            return Err(response.into());
        }
        println!(
            "{}",
            response
                .split_once("\r\n\r\n")
                .map_or("Report saved", |(_, body)| body)
        );
        return Ok(());
    }
    let listener = match tokio::net::TcpListener::bind("127.0.0.1:4317").await {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            let mut socket = tokio::net::TcpStream::connect("127.0.0.1:4317").await?;
            socket
                .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1:4317\r\nConnection: close\r\n\r\n")
                .await?;
            let mut response = Vec::new();
            tokio::time::timeout(
                Duration::from_secs(3),
                socket.take(4096).read_to_end(&mut response),
            )
            .await??;
            let response = String::from_utf8_lossy(&response);
            if !response.contains("<title>Goal Layers</title>")
                && !response.contains("<title>Goal Atlas</title>")
            {
                return Err(error.into());
            }
            open_ui();
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let file = std::env::var_os("GOAL_ATLAS_DATA")
        .map(PathBuf::from)
        .unwrap_or(
            std::env::current_exe()?
                .with_file_name("data")
                .join("nodes.json"),
        );
    tokio::fs::create_dir_all(file.parent().ok_or("Data file needs a parent")?).await?;
    let nodes = match tokio::fs::read(&file).await {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    let (updates, _) = broadcast::channel(16);
    let flow = flow::Hub::load(file.with_file_name("flows.json")).await?;
    let app = Arc::new(App {
        trace: Arc::new(std::sync::Mutex::new(trace::Cache::default())),
        flow,
        codex: Mutex::new(Codex::connect().await?),
        catalog: RwLock::new(Catalog::default()),
        nodes: Mutex::new(nodes),
        message_receipts: Mutex::new(
            match tokio::fs::read(file.with_file_name("message-receipts.json")).await {
                Ok(bytes) => serde_json::from_slice(&bytes)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
                Err(e) => return Err(e.into()),
            },
        ),
        reports: RwLock::new(BTreeMap::new()),
        codex_home: std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join(".codex")
            }),
        file,
        updates,
    });
    let worker = tokio::spawn(synchronize(app.clone()));
    let routes = Router::new()
        .route("/", get(account::page))
        .route(
            "/vendor/markdown-it.min.js",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/javascript")],
                    include_str!("../web/vendor/markdown-it.min.js"),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/javascript")],
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/css")],
                    include_str!("../web/style.css"),
                )
            }),
        )
        .route(
            "/logo.svg",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "image/svg+xml")],
                    include_str!("../web/logo.svg"),
                )
            }),
        )
        .route(
            "/favicon.ico",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "image/x-icon")],
                    include_bytes!("../web/favicon.ico").as_slice(),
                )
            }),
        )
        .route(
            "/icon-192.png",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "image/png")],
                    include_bytes!("../web/icon-192.png").as_slice(),
                )
            }),
        )
        .route(
            "/apple-touch-icon.png",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "image/png")],
                    include_bytes!("../web/apple-touch-icon.png").as_slice(),
                )
            }),
        )
        .route("/api/catalog", get(catalog))
        .route("/api/account", get(account::read))
        .route("/api/thread/{id}", get(detail))
        .route("/api/trace/{id}", get(scope_trace))
        .route("/api/thread/{id}/message", post(chat_actions::send))
        .route("/api/thread/{id}/review/stop", post(fork_review::stop))
        .route("/api/nodes", post(edit_node))
        .route("/api/nodes/visibility", post(node_visibility))
        .route("/api/nodes/visibility/bulk", post(bulk_node_visibility))
        .route("/api/reports", post(report))
        .route("/api/flow/{id}", get(flow::get))
        .route("/api/flow/{id}/reconstruct", post(flow::start))
        .route("/api/flow/{id}/cancel", post(flow::cancel))
        .route("/api/flow/{id}/review", post(flow::review))
        .route(
            "/fonts/DMSans.ttf",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "font/ttf")],
                    include_bytes!("../web/fonts/DMSans.ttf").as_slice(),
                )
            }),
        )
        .route(
            "/fonts/SpaceGrotesk.ttf",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "font/ttf")],
                    include_bytes!("../web/fonts/SpaceGrotesk.ttf").as_slice(),
                )
            }),
        )
        .route(
            "/flow.js",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/javascript")],
                    include_str!("../web/flow.js"),
                )
            }),
        )
        .route(
            "/events",
            get(|State(app): State<Arc<App>>| async move {
                let stream = BroadcastStream::new(app.updates.subscribe()).map(|event| {
                    Ok::<_, Infallible>(
                        Event::default()
                            .data(event.map_or_else(|_| "resync".into(), |n| n.to_string())),
                    )
                });
                Sse::new(stream).keep_alive(
                    axum::response::sse::KeepAlive::new().interval(Duration::from_secs(10)),
                )
            }),
        )
        .layer(DefaultBodyLimit::max(32 * 1024))
        .layer(axum::middleware::from_fn(local_only))
        .with_state(app);
    println!("Goal Layers: http://127.0.0.1:4317 — local Codex connection");
    open_ui();
    axum::serve(listener, routes)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    worker.abort();
    let _ = worker.await;
    Ok(())
}

fn open_ui() {
    if cfg!(windows) && !std::env::args().any(|a| a == "--no-open") {
        let _ = std::process::Command::new("rundll32.exe")
            .args(["url.dll,FileProtocolHandler", "http://127.0.0.1:4317"])
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn node(id: &str, parent: Option<&str>) -> Node {
        Node {
            id: id.into(),
            thread_id: "goal".into(),
            parent_id: parent.map(str::to_owned),
            title: "Step".into(),
            status: "pending".into(),
            lane: "aligned".into(),
            evidence: String::new(),
            next: String::new(),
            revision: 0,
            acceptance: String::new(),
            blockers: Vec::new(),
            reported_evidence: String::new(),
            status_report_id: None,
            hidden: false,
        }
    }
    #[test]
    fn tree_rejects_cycles_and_cross_goal_parents() {
        let root = node("root", None);
        let child = node("child", Some("root"));
        assert!(validate(std::slice::from_ref(&root), &child).is_ok());
        assert!(validate(&[root, child], &node("root", Some("child"))).is_err());
        assert!(validate(&[], &node("orphan", Some("missing"))).is_err());
    }
    #[test]
    fn completion_requires_evidence_and_stale_writes_fail() {
        let mut n = node("x", None);
        n.status = "unknown".into();
        assert!(validate(&[], &n).is_ok());
        n.status = "complete".into();
        assert!(validate(&[], &n).is_err());
        n.evidence = "Observed test result".into();
        assert!(validate(&[], &n).is_ok());
        let mut newer = n.clone();
        newer.revision = 1;
        assert!(validate(&[newer], &n).is_err());
    }
    #[test]
    fn manual_edits_cannot_create_or_reparent_steps() {
        let root = node("root", None);
        assert!(validate_edit(&[], &root).is_err());
        assert!(validate_edit(std::slice::from_ref(&root), &root).is_ok());
        assert!(validate_edit(&[root], &node("root", Some("other"))).is_err());
        assert!(validate(&[], &node("agent-step", None)).is_ok());
    }
    #[test]
    fn imports_latest_report_without_claiming_verification() {
        let rows = vec![
            json!({"item":{"type":"agentMessage","id":"source","text":"Scope check:\n| Requirement | Status | Evidence | Cause / correction |\n| --- | --- | --- | --- |\n| R1 App | DONE | Test passed | None |\n| R2 Work | DRIFT | Unrequested | Return to scope |\n| R3 Proof | UNVERIFIED | Missing | Run test |"}}),
        ];
        let nodes = reported_steps(&rows, "goal");
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].status, "complete");
        assert!(nodes[0].evidence.contains("not independently verified"));
        assert_eq!(nodes[1].lane, "drift");
        assert_eq!(nodes[2].status, "pending");
        assert!(
            reported_steps(
                &[json!({"type":"agentMessage","text":"Everything is done!"})],
                "goal"
            )
            .is_empty()
        );
    }
    #[test]
    fn registered_worktrees_belong_to_their_project() {
        let root =
            std::env::temp_dir().join(format!("atlas-worktrees-{}-{}", std::process::id(), now()));
        let record = root.join("repository/.git/worktrees/branch");
        std::fs::create_dir_all(&record).unwrap();
        std::fs::write(
            root.join("repository/.git/config"),
            "[remote \"origin\"]\nurl = https://example.com/repository.git\n",
        )
        .unwrap();
        let checkout = root.join("separate-checkout");
        std::fs::write(
            record.join("gitdir"),
            checkout.join(".git").to_string_lossy().as_bytes(),
        )
        .unwrap();
        let mut project =
            json!({"id":"project","roots":[{"path":root.join("repository").to_string_lossy()}]});
        // The configured repository is the metadata owner, independent of checkout naming.
        add_worktree_roots(&mut project);
        assert_eq!(
            project_for(
                &json!({"cwd":"missing","repositoryUrl":"https://example.com/repository.git"}),
                &[project.clone()]
            ),
            Some("project".into())
        );
        assert_eq!(
            project_for(
                &json!({"repositoryUrl":"https://example.com/repository.git"}),
                &[project.clone(), project.clone()]
            ),
            None
        );
        assert_eq!(project["worktreeRoots"].as_array().unwrap().len(), 1);
        assert_eq!(
            project_for(
                &json!({"cwd":checkout.join("src").to_string_lossy()}),
                &[project]
            ),
            Some("project".into())
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn groups_legacy_chats_by_longest_root_without_prefix_collisions() {
        let projects = vec![
            json!({"id":"a","roots":[{"path":"E:\\Code\\App"}]}),
            json!({"id":"b","roots":[{"path":"E:\\Code\\App\\nested"}]}),
        ];
        assert_eq!(
            project_for(&json!({"cwd":"e:/code/app/src"}), &projects),
            Some("a".into())
        );
        assert_eq!(
            project_for(&json!({"cwd":"E:/Code/App/nested/src"}), &projects),
            Some("b".into())
        );
        assert_eq!(
            project_for(&json!({"cwd":"E:/Code/Application"}), &projects),
            None
        );
        assert_eq!(
            project_for(
                &json!({"cwd":"\\\\?\\E:\\Code\\App\\src","projectId":""}),
                &projects
            ),
            Some("a".into())
        );
        for path in [
            "/mnt/e/Code/App",
            "C:\\mnt\\e\\code\\app",
            "C:/mnt/e/Code/App/nested/src",
        ] {
            assert_eq!(
                project_for(&json!({"cwd":path}), &projects),
                Some(if path.ends_with("src") { "b" } else { "a" }.into())
            );
        }
        for path in [
            "C:/mnt/e/Code/Application",
            "C:/mnt/extra/Code/App",
            "/other/e/Code/App",
        ] {
            assert_eq!(project_for(&json!({"cwd":path}), &projects), None);
        }
        assert_eq!(
            project_for(
                &json!({"cwd":"C:/mnt/e/Code/App","projectId":"explicit"}),
                &projects
            ),
            Some("explicit".into())
        );
        let ambiguous = vec![
            projects[0].clone(),
            json!({"id":"duplicate","roots":[{"path":"/mnt/e/Code/App"}]}),
        ];
        assert_eq!(
            project_for(&json!({"cwd":"C:/mnt/e/Code/App"}), &ambiguous),
            None
        );
    }
}
