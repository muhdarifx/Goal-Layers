//! Apply only newer, node-specific recorded evidence; uncertainty goes to explicit Refresh.
use crate::*;

fn event_time(value: &Value) -> Option<u64> {
    let s = value.as_str()?;
    if !s.ends_with('Z')
        || s.get(4..5) != Some("-")
        || s.get(7..8) != Some("-")
        || s.get(10..11) != Some("T")
        || s.get(13..14) != Some(":")
        || s.get(16..17) != Some(":")
        || (s.len() != 20 && s.as_bytes().get(19) != Some(&b'.'))
    {
        return None;
    }
    let number = |a, b| s.get(a..b)?.parse::<u64>().ok();
    let (y, m, d, h, min, sec) = (
        number(0, 4)?,
        number(5, 7)?,
        number(8, 10)?,
        number(11, 13)?,
        number(14, 16)?,
        number(17, 19)?,
    );
    if !(1970..=9999).contains(&y) || !(1..=12).contains(&m) || h > 23 || min > 59 || sec > 59 {
        return None;
    }
    let leap = |y: u64| y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let months = [
        31,
        28 + u64::from(leap(y)),
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if d == 0 || d > months[(m - 1) as usize] {
        return None;
    }
    let days = (1970..y)
        .map(|year| 365 + u64::from(leap(year)))
        .sum::<u64>()
        + months[..(m - 1) as usize].iter().sum::<u64>()
        + d
        - 1;
    let millis = if s.as_bytes().get(19) == Some(&b'.') {
        let fraction = s.get(20..s.len() - 1)?;
        if fraction.is_empty() || !fraction.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        format!("{fraction:0<3}")[..3].parse::<u64>().ok()?
    } else {
        0
    };
    Some(((days * 24 + h) * 60 + min) * 60000 + sec * 1000 + millis)
}

fn observation(progress: &Value) -> Option<(u64, String, String)> {
    let mut found = None;
    let report = &progress["latest_report"];
    if let Some(at) = event_time(&report["at"]) {
        let status = text(report, "status");
        if matches!(status, "pending" | "in_progress" | "blocked" | "complete") {
            found = Some((
                at,
                status.to_owned(),
                format!(
                    "Recorded status report {} at {}",
                    text(report, "id"),
                    text(report, "at")
                ),
            ));
        }
    }
    // A general passing command does not establish that this node is complete.
    if !text(progress, "acceptance_criteria").is_empty()
        && matches!(
            text(progress, "verification"),
            "verified" | "check_failed" | "needs_recheck"
        )
    {
        let check = if progress["verification"] == "needs_recheck" {
            &progress["latest_edit"]
        } else {
            &progress["latest_check"]
        };
        if let Some(at) = event_time(&check["at"]) {
            if found.as_ref().is_none_or(|(previous, _, _)| at > *previous) {
                found = Some((
                    at,
                    if progress["verification"] == "verified" {
                        "complete"
                    } else {
                        "unknown"
                    }
                    .into(),
                    format!(
                        "Recorded acceptance evidence: {} ({}, {})",
                        text(progress, "acceptance_criteria"),
                        text(check, "id"),
                        text(progress, "verification")
                    ),
                ));
            }
        }
    }
    found
}

pub async fn refresh(app: &Arc<App>, thread: &str) -> Result<Vec<String>, ApiError> {
    let mut known = app
        .flow
        .documents
        .lock()
        .await
        .get(thread)
        .map(|d| d.nodes())
        .unwrap_or_default();
    for node in app
        .nodes
        .lock()
        .await
        .iter()
        .filter(|n| n.thread_id == thread)
    {
        known.retain(|n| n.id != node.id);
        known.push(node.clone());
    }
    known.retain(|n| n.id != "original-goal");
    let trace = crate::scope_trace(State(app.clone()), Path(thread.to_owned()))
        .await
        .map(|v| v.0)
        .unwrap_or_default();
    let mut receipts = app.message_receipts.lock().await;
    let mut stored = app.nodes.lock().await;
    let mut next = stored.clone();
    let mut unresolved = Vec::new();
    let mut stamps = Vec::new();
    for node in known {
        let Some((at, status, evidence)) = observation(&trace["nodes"][&node.id]["progress"])
        else {
            unresolved.push(node.id);
            continue;
        };
        let cutoff = node
            .status_report_id
            .as_ref()
            .and_then(|id| {
                id.strip_prefix("manual-")
                    .and_then(|s| s.parse().ok())
                    .or_else(|| {
                        receipts
                            .get(&format!("{thread}:{id}"))
                            .and_then(|r| r["requested_at"].as_u64())
                    })
            })
            .unwrap_or(0);
        if at < cutoff {
            unresolved.push(node.id);
            continue;
        }
        if at == cutoff {
            if node.status != status || status == "unknown" {
                unresolved.push(node.id);
            }
            continue;
        }
        if stored
            .iter()
            .find(|n| n.thread_id == thread && n.id == node.id)
            .is_some_and(|n| n != &node)
        {
            unresolved.push(node.id);
            continue;
        }
        let mut updated = node.clone();
        if status == "unknown" {
            unresolved.push(node.id.clone());
        }
        updated.status = status;
        updated.reported_evidence = evidence;
        if updated.status != "blocked" {
            updated.blockers.clear();
        } else if updated.blockers.is_empty() {
            updated
                .blockers
                .push("See the latest recorded status report".into());
        }
        updated.revision = updated
            .revision
            .checked_add(1)
            .ok_or_else(|| api_error("Node revision exhausted"))?;
        let source = format!("local-{}", node.id);
        updated.status_report_id = Some(source.clone());
        next.retain(|n| n.thread_id != thread || n.id != updated.id);
        next.push(updated);
        stamps.push((
            format!("{thread}:{source}"),
            json!({"thread_id":thread,"action":"local_status","requested_at":at}),
        ));
    }
    if next != *stored {
        let temp = app.file.with_extension("tmp");
        tokio::fs::write(&temp, serde_json::to_vec_pretty(&next).map_err(api_error)?)
            .await
            .map_err(api_error)?;
        tokio::fs::rename(temp, &app.file)
            .await
            .map_err(api_error)?;
        *stored = next;
        for (key, value) in stamps {
            receipts.insert(key, value);
        }
        chat_actions::save_receipts(app, &receipts).await?;
        let _ = app.updates.send(now());
    }
    Ok(unresolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn newer_reports_update_saved_nodes_once_and_preserve_manual_fields() {
        let dir = std::env::temp_dir().join(format!("live-status-{}", now()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.jsonl");
        let event = |at: &str, id: &str, status: &str| json!({"timestamp":at,"payload":{"type":"item_completed","thread_id":"t","turn_id":"turn","item":{"id":id,"type":"AgentMessage","text":format!("Scope check:\n| Requirement | Status | Evidence | Cause / correction |\n| R1 | {status} | Recorded evidence | Next action |")}}});
        std::fs::write(
            &path,
            format!("{}\n", event("2026-01-01T00:00:00Z", "first", "DONE")),
        )
        .unwrap();
        let mut child = Command::new("node")
            .args(["-e", "process.stdin.resume()"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut catalog = Catalog::default();
        catalog.threads.insert("t".into(), json!({"path":path}));
        let (updates, _) = broadcast::channel(4);
        let app = Arc::new(App {
            trace: Arc::new(std::sync::Mutex::new(trace::Cache::default())),
            flow: flow::Hub::load(dir.join("flows.json")).await.unwrap(),
            codex: Mutex::new(Codex {
                input: child.stdin.take().unwrap(),
                output: BufReader::new(child.stdout.take().unwrap()).lines(),
                _child: child,
                sequence: 0,
            }),
            catalog: RwLock::new(catalog),
            nodes: Mutex::new(vec![]),
            message_receipts: Mutex::new(BTreeMap::new()),
            reports: RwLock::new(BTreeMap::new()),
            codex_home: dir.clone(),
            file: dir.join("nodes.json"),
            updates,
        });
        let graph:flow::Graph=serde_json::from_value(json!({"summary":"Test","nodes":[{"id":"n","title":"R1","kind":"Scope detail","lane":"aligned","status":"pending","description":"Keep me","next":"Next action","evidence":[]}],"edges":[],"inspected_files":[],"limitations":[]})).unwrap();
        app.flow.documents.lock().await.insert(
            "t".into(),
            flow::Document {
                thread_id: "t".into(),
                generated_at: 0,
                source_count: 1,
                source_threads: 1,
                graph,
                mode: "local".into(),
            },
        );
        assert!(refresh(&app, "t").await.unwrap().is_empty());
        let saved = app.nodes.lock().await[0].clone();
        assert_eq!(saved.status, "complete");
        assert!(saved.evidence.contains("Keep me"));
        assert!(refresh(&app, "t").await.unwrap().is_empty());
        assert_eq!(saved, app.nodes.lock().await[0]);
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            file,
            "{}",
            event("2025-12-31T00:00:00Z", "late-old", "ON TRACK")
        )
        .unwrap();
        refresh(&app, "t").await.unwrap();
        assert_eq!(saved, app.nodes.lock().await[0]);
        writeln!(file, "{}", event("2026-01-02T00:00:00Z", "new", "ON TRACK")).unwrap();
        refresh(&app, "t").await.unwrap();
        assert_eq!(app.nodes.lock().await[0].status, "in_progress");
        assert_eq!(app.nodes.lock().await[0].evidence, saved.evidence);
        drop(file);
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn only_explicit_reports_or_exact_verified_checks_establish_status() {
        assert!(observation(&json!({"verification":"checks_passed","latest_check":{"at":"2026-01-01T00:00:00Z"}})).is_none());
        let report =
            json!({"latest_report":{"at":"2026-01-01T00:00:00.123Z","id":"r","status":"complete"}});
        assert_eq!(observation(&report).unwrap().1, "complete");
        let invalidated = json!({"acceptance_criteria":"cargo test","verification":"needs_recheck","latest_check":{"at":"2026-01-01T00:00:00Z"}});
        assert!(observation(&invalidated).is_none());
        let valid = json!({"acceptance_criteria":"cargo test","verification":"verified","latest_check":{"at":"2026-01-01T00:00:01Z","id":"c"}});
        assert_eq!(observation(&valid).unwrap().1, "complete");
        assert_eq!(event_time(&json!("1970-01-01T00:00:00Z")), Some(0));
        assert_eq!(
            event_time(&json!("1970-01-02T00:00:00.123Z")),
            Some(86400123)
        );
        assert!(event_time(&json!("2026-02-30T00:00:00Z")).is_none());
    }
}
