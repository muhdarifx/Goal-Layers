//! Incremental, read-only token and source trace from the selected Codex session.
use crate::{flow::Graph, text};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::Path,
};

#[derive(Default)]
pub struct Cache {
    files: BTreeMap<String, Session>,
}
#[derive(Default)]
struct Session {
    offset: u64,
    seen: HashSet<String>,
    turns: BTreeMap<String, Value>,
    items: BTreeMap<String, Value>,
    human: Option<String>,
    sequence: u64,
}
fn event_order(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a["at"].as_str(), b["at"].as_str()) {
        (Some(a_time), Some(b_time)) if a_time != b_time => a_time.cmp(b_time),
        _ => a["sequence"].as_u64().cmp(&b["sequence"].as_u64()),
    }
}
impl Session {
    fn ingest(&mut self, event: &Value) {
        let p = &event["payload"];
        if text(event, "type") == "token_usage_record" {
            let turn = text(p, "turn_id");
            let response = text(p, "response_id");
            if turn.is_empty() || response.is_empty() || self.seen.contains(response) {
                return;
            }
            let usage = &p["usage"];
            if usage["total_tokens"].as_u64().is_none() {
                return;
            }
            self.seen.insert(response.into());
            let entry = self
                .turns
                .entry(turn.into())
                .or_insert_with(|| json!({"tokens":0,"input":0,"cached":0,"output":0}));
            for (to, from) in [
                ("tokens", "total_tokens"),
                ("input", "input_tokens"),
                ("cached", "cached_input_tokens"),
                ("output", "output_tokens"),
            ] {
                entry[to] = json!(
                    entry[to]
                        .as_u64()
                        .unwrap_or(0)
                        .saturating_add(usage[from].as_u64().unwrap_or(0))
                );
            }
        } else if text(p, "type") == "item_completed" {
            let item = &p["item"];
            let id = text(item, "id");
            if id.is_empty() {
                return;
            }
            if self.items.contains_key(id) {
                return;
            }
            let kind = text(item, "type");
            self.sequence += 1;
            let human = matches!(kind, "UserMessage" | "userMessage" | "user_message");
            if human {
                let content = crate::flow::searchable(&item["content"]);
                if !content.trim_start().starts_with('<') && !content.starts_with("# AGENTS.md") {
                    self.human = Some(content.chars().take(600).collect());
                }
            }
            let command = crate::flow::searchable(&item["command"]);
            let exact_command = (command.len() <= 4000).then(|| command.clone());
            let check = [
                "cargo test",
                "cargo check",
                "cargo clippy",
                "npm test",
                "npm run test",
                "npm run build",
                "pnpm test",
                "pnpm build",
                "playwright test",
                "node --test",
            ]
            .iter()
            .any(|s| command.contains(s));
            let event_kind = if human {
                "request"
            } else if matches!(
                kind,
                "CommandExecution" | "commandExecution" | "command_execution"
            ) {
                if check { "check" } else { "command" }
            } else if matches!(kind, "FileChange" | "fileChange" | "file_change") {
                "edit"
            } else if matches!(kind, "AgentMessage" | "agentMessage" | "agent_message") {
                "report"
            } else {
                "other"
            };
            let summary = if event_kind == "check" || event_kind == "command" {
                command
            } else if event_kind == "edit" {
                crate::flow::searchable(&item["changes"])
            } else if human {
                crate::flow::searchable(&item["content"])
            } else {
                if event_kind == "other" {
                    String::new()
                } else {
                    text(item, "text").to_owned()
                }
            };
            let reports: Vec<Value> = if event_kind == "report" {
                crate::reported_steps(
                    &[json!({"type":"agentMessage","text":text(item,"text")})],
                    "",
                )
                .into_iter()
                .map(|r| json!({"title":r.title,"status":r.status}))
                .collect()
            } else {
                Vec::new()
            };
            self.items.insert(id.into(),json!({"id":id,"turn_id":p["turn_id"],"at":event["timestamp"],"sequence":self.sequence,"kind":event_kind,"command":exact_command,"reports":reports,"summary":summary.chars().take(1200).collect::<String>(),"exit_code":item.get("exit_code").or_else(||item.get("exitCode")),"origin":if human{"human"}else{"agent"},"preceding_request":self.human}));
        }
    }
}
impl Cache {
    pub fn read(
        &mut self,
        path: &Path,
        thread_id: &str,
        graph: Option<&Graph>,
        criteria: &BTreeMap<String, String>,
    ) -> Result<Value, String> {
        let key = path.to_string_lossy().into_owned();
        if self.files.len() >= 8 && !self.files.contains_key(&key) {
            self.files.clear();
        }
        let mut file = File::open(path).map_err(|_| "Session log unavailable")?;
        let size = file
            .metadata()
            .map_err(|_| "Session log unavailable")?
            .len();
        let state = self.files.entry(key).or_default();
        if size < state.offset {
            *state = Session::default();
        }
        file.seek(SeekFrom::Start(state.offset))
            .map_err(|_| "Could not read session log")?;
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = reader
                .read_until(b'\n', &mut line)
                .map_err(|_| "Could not read session log")?;
            if n == 0 || !line.ends_with(b"\n") {
                break;
            }
            state.offset += n as u64;
            if let Ok(event) = serde_json::from_slice::<Value>(&line) {
                if event["payload"]["thread_id"]
                    .as_str()
                    .is_none_or(|id| id == thread_id)
                {
                    state.ingest(&event);
                }
            }
        }
        let mut nodes = BTreeMap::new();
        for node in graph.into_iter().flat_map(|g| &g.nodes) {
            let records: Vec<_> = node
                .evidence
                .iter()
                .filter_map(|e| {
                    e.source_id
                        .split_once(':')
                        .and_then(|(_, id)| state.items.get(id))
                })
                .collect();
            let turns: HashSet<_> = records
                .iter()
                .filter_map(|r| r["turn_id"].as_str())
                .collect();
            let usage: Vec<_> = turns.iter().filter_map(|id| state.turns.get(*id)).collect();
            let first = records
                .iter()
                .min_by_key(|r| r["at"].as_str().unwrap_or(""));
            let mut timeline: Vec<_> = state
                .items
                .values()
                .filter(|r| turns.contains(text(r, "turn_id")) && text(r, "kind") != "other")
                .cloned()
                .collect();
            timeline.sort_by(event_order);
            let criterion = criteria.get(&node.id).map(String::as_str).unwrap_or("");
            let check = if criterion.is_empty() {
                timeline
                    .iter()
                    .rev()
                    .find(|r| r["kind"] == "check" && r["exit_code"].as_i64().is_some())
            } else {
                state
                    .items
                    .values()
                    .filter(|r| r["command"] == criterion && r["exit_code"].as_i64().is_some())
                    .max_by(|a, b| event_order(a, b))
            };
            let newer_work = check.is_some_and(|check| {
                state
                    .items
                    .values()
                    .any(|r| r["kind"] == "edit" && event_order(r, check).is_gt())
            });
            let verification = if newer_work {
                "needs_recheck"
            } else if let Some(check) = check {
                if check["exit_code"] == 0 {
                    if criterion.is_empty() {
                        "checks_passed"
                    } else {
                        "verified"
                    }
                } else {
                    "check_failed"
                }
            } else {
                "not_verified"
            };
            let latest_report = state
                .items
                .values()
                .filter(|r| {
                    r["reports"].as_array().is_some_and(|reports| {
                        reports.iter().any(|report| report["title"] == node.title)
                    })
                })
                .max_by(|a, b| event_order(a, b));
            let reported_status = latest_report
                .and_then(|r| r["reports"].as_array())
                .and_then(|reports| reports.iter().find(|r| r["title"] == node.title))
                .and_then(|r| r["status"].as_str())
                .unwrap_or(&node.status);
            let progress = json!({"reported_status":reported_status,"verification":verification,"acceptance_criteria":criterion,"latest_check":check,"latest_edit":state.items.values().filter(|r|r["kind"]=="edit").max_by(|a,b|event_order(a,b)),"latest_report":latest_report.map(|r|json!({"id":r["id"],"at":r["at"],"status":reported_status})),"timeline":timeline.iter().rev().take(30).collect::<Vec<_>>(),"limitation":"Verification means the defined acceptance command passed in this recorded session. It does not certify untested behavior. Newer edits require reassessment."});
            let sum = |key: &str| {
                usage
                    .iter()
                    .map(|v| v[key].as_u64().unwrap_or(0))
                    .fold(0u64, u64::saturating_add)
            };
            nodes.insert(node.id.clone(),json!({"origin":if records.len()==node.evidence.len()&&!records.is_empty()&&records.iter().all(|r|r["origin"]=="human"){"human"}else if records.len()==node.evidence.len()&&!records.is_empty()&&records.iter().all(|r|r["origin"]=="agent"){"agent"}else{"unknown"},"first_seen":first.map(|r|&r["at"]),"preceding_request":first.map(|r|&r["preceding_request"]),"tokens":if records.len()==node.evidence.len()&&usage.len()==turns.len()&&!turns.is_empty(){Some(sum("tokens"))}else{None},"input":sum("input"),"cached":sum("cached"),"output":sum("output"),"turn_count":turns.len(),"attribution":"Shared source-turn usage, not exclusive node usage"}));
            nodes.get_mut(&node.id).unwrap()["progress"] = progress;
        }
        let total = state
            .turns
            .values()
            .map(|v| v["tokens"].as_u64().unwrap_or(0))
            .fold(0u64, u64::saturating_add);
        Ok(
            json!({"available":true,"recorded_tokens":total,"recorded_turns":state.turns.len(),"nodes":nodes}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completion_requires_exact_check_and_newer_edits_invalidate_it() {
        let dir = std::env::temp_dir().join(format!(
            "atlas-proof-{}-{}",
            std::process::id(),
            crate::now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.jsonl");
        let event = |id: &str, kind: &str, exit: Value| json!({"timestamp":"2026-01-01T00:00:00Z","payload":{"type":"item_completed","turn_id":"turn","item":{"id":id,"type":kind,"command":["cargo","test"],"exit_code":exit}}});
        let report = event("report", "AgentMessage", Value::Null);
        let pass = event("check", "CommandExecution", json!(0));
        std::fs::write(&path, format!("{report}\n{pass}\n{pass}\n")).unwrap();
        let graph = Graph {
            summary: String::new(),
            nodes: vec![crate::flow::Component {
                id: "step".into(),
                title: "Feature".into(),
                kind: "Reported step".into(),
                lane: "aligned".into(),
                status: "complete".into(),
                description: String::new(),
                next: String::new(),
                evidence: vec![crate::flow::Evidence {
                    source_id: "t:report".into(),
                    quote: "report".into(),
                }],
            }],
            edges: vec![],
            inspected_files: vec![],
            limitations: vec![],
        };
        let mut cache = Cache::default();
        let criteria = BTreeMap::from([("step".into(), "cargo test".into())]);
        assert_eq!(
            cache
                .read(&path, "t", Some(&graph), &BTreeMap::new())
                .unwrap()["nodes"]["step"]["progress"]["verification"],
            "checks_passed"
        );
        assert_eq!(
            cache.read(&path, "t", Some(&graph), &criteria).unwrap()["nodes"]["step"]["progress"]["verification"],
            "verified"
        );
        assert_eq!(cache.files.values().next().unwrap().items.len(), 2);
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, "{}", event("edit", "FileChange", Value::Null)).unwrap();
        assert_eq!(
            cache.read(&path, "t", Some(&graph), &criteria).unwrap()["nodes"]["step"]["progress"]["verification"],
            "needs_recheck"
        );
        writeln!(f, "{}", event("failed", "CommandExecution", json!(1))).unwrap();
        assert_eq!(
            cache.read(&path, "t", Some(&graph), &criteria).unwrap()["nodes"]["step"]["progress"]["verification"],
            "check_failed"
        );
        let mut old_pass = event("late-arriving-old-pass", "CommandExecution", json!(0));
        old_pass["timestamp"] = json!("2025-12-31T23:00:00Z");
        writeln!(f, "{}", old_pass).unwrap();
        writeln!(
            f,
            "{}",
            event("missing-exit", "CommandExecution", Value::Null)
        )
        .unwrap();
        assert_eq!(
            cache.read(&path, "t", Some(&graph), &criteria).unwrap()["nodes"]["step"]["progress"]["verification"],
            "check_failed"
        );
        drop(f);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn native_human_items_retain_their_origin_and_context() {
        let mut s = Session::default();
        s.ingest(&json!({"timestamp":"2026-01-01T00:00:00Z","payload":{"type":"item_completed","turn_id":"turn","item":{"id":"human","type":"UserMessage","content":[{"text":"Please build the requested feature"}]}}}));
        assert_eq!(s.items["human"]["origin"], "human");
        assert!(
            s.items["human"]["preceding_request"]
                .as_str()
                .unwrap()
                .contains("Please build")
        );
    }
    #[test]
    fn appended_partial_records_and_truncation_are_handled() {
        let dir = std::env::temp_dir().join(format!(
            "atlas-trace-{}-{}",
            std::process::id(),
            crate::now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("trace.jsonl");
        let event=json!({"type":"token_usage_record","payload":{"thread_id":"t","turn_id":"turn","response_id":"r","usage":{"total_tokens":50}}}).to_string();
        std::fs::write(&path, &event).unwrap();
        let mut c = Cache::default();
        assert_eq!(
            c.read(&path, "t", None, &BTreeMap::new()).unwrap()["recorded_tokens"],
            0
        );
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        assert_eq!(
            c.read(&path, "t", None, &BTreeMap::new()).unwrap()["recorded_tokens"],
            50
        );
        assert_eq!(
            c.read(&path, "t", None, &BTreeMap::new()).unwrap()["recorded_tokens"],
            50
        );
        std::fs::write(&path, "").unwrap();
        assert_eq!(
            c.read(&path, "t", None, &BTreeMap::new()).unwrap()["recorded_tokens"],
            0
        );
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
    #[test]
    fn token_records_are_deduplicated_and_missing_usage_is_not_zero() {
        let mut s = Session::default();
        let event = json!({"type":"token_usage_record","payload":{"turn_id":"turn","response_id":"r","usage":{"total_tokens":50,"input_tokens":40,"cached_input_tokens":20,"output_tokens":10}}});
        s.ingest(&event);
        s.ingest(&event);
        assert_eq!(s.turns["turn"]["tokens"], 50);
        assert_eq!(s.turns["turn"]["cached"], 20);
        assert!(s.turns.get("missing").is_none());
    }
}
