use crate::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusReport {
    version: u8,
    thread_id: String,
    request_id: String,
    updates: Vec<Update>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    node_id: String,
    status: String,
    blockers: Vec<String>,
    next_step: String,
    evidence: String,
}
fn parse(text: &str) -> Result<StatusReport, String> {
    let normalized = text.replace("\r\n", "\n");
    let text = normalized.as_str();
    let body = text
        .split_once("```goal-atlas-status\n")
        .ok_or("Missing structured status block")?
        .1;
    let json = body
        .split_once("```")
        .ok_or("Incomplete structured status block")?
        .0;
    if json.len() > 128000 {
        return Err("Status response too large".into());
    }
    serde_json::from_str(json).map_err(|_| "Invalid structured status response".into())
}
fn patches(
    report: &StatusReport,
    thread: &str,
    targets: &[Node],
    known: &[Node],
) -> Result<Vec<Node>, String> {
    if report.version != 1
        || report.thread_id != thread
        || report.updates.len() != targets.len()
        || targets.is_empty()
    {
        return Err("Status response does not match the requested selection".into());
    }
    let mut ids = HashSet::new();
    let mut result = Vec::new();
    for update in &report.updates {
        if !ids.insert(&update.node_id) {
            return Err("Duplicate node update".into());
        }
        let baseline = targets
            .iter()
            .find(|n| n.id == update.node_id)
            .ok_or("Unrequested node update")?;
        let current = known
            .iter()
            .find(|n| n.id == update.node_id && n.thread_id == thread)
            .ok_or("Node no longer exists")?;
        if current != baseline {
            return Err(
                "Node changed while awaiting the status response; request a fresh status".into(),
            );
        }
        if !matches!(
            update.status.as_str(),
            "unknown" | "pending" | "in_progress" | "blocked" | "complete"
        ) || update.blockers.len() > 20
            || update.blockers.iter().any(|s| s.len() > 1000)
            || update.next_step.len() > 4000
            || update.evidence.len() > 12000
            || update.evidence.trim().is_empty()
        {
            return Err("Invalid status fields or missing evidence".into());
        }
        if update.status == "blocked" && update.blockers.is_empty() {
            return Err("Blocked status requires a blocker".into());
        }
        let mut node = current.clone();
        node.status = update.status.clone();
        node.blockers = update.blockers.clone();
        node.next = update.next_step.clone();
        node.reported_evidence = update.evidence.clone();
        node.status_report_id = Some(report.request_id.clone());
        node.revision = node
            .revision
            .checked_add(1)
            .ok_or("Node revision exhausted")?;
        result.push(node);
    }
    Ok(result)
}

pub async fn apply(app: &Arc<App>, thread: &str, rows: &[Value]) -> Result<Vec<Value>, ApiError> {
    let imported = app
        .flow
        .documents
        .lock()
        .await
        .get(thread)
        .map(|d| d.nodes())
        .unwrap_or_default();
    let mut receipts = app.message_receipts.lock().await;
    let mut outcomes = Vec::new();
    for row in rows {
        let item = row.get("item").unwrap_or(row);
        if text(item, "type") != "agentMessage" {
            continue;
        }
        let Ok(report) = parse(text(item, "text")) else {
            continue;
        };
        let key = format!("{thread}:{}", report.request_id);
        let Some(receipt) = receipts.get(&key).cloned() else {
            continue;
        };
        if receipt["action"] != "status" {
            continue;
        }
        if matches!(
            text(&receipt, "status"),
            "applied" | "rejected" | "superseded"
        ) {
            outcomes.push(receipt);
            continue;
        }
        let targets: Vec<Node> =
            serde_json::from_value(receipt["targets"].clone()).map_err(api_error)?;
        let newer = receipts.values().any(|r| {
            r["action"] == "status"
                && r["thread_id"] == thread
                && r["requested_at"].as_u64() > receipt["requested_at"].as_u64()
                && r["targets"].as_array().is_some_and(|nodes| {
                    nodes
                        .iter()
                        .any(|n| targets.iter().any(|t| n["id"] == t.id))
                })
        });
        let mut stored = app.nodes.lock().await;
        let mut known = imported.clone();
        for n in stored.iter().filter(|n| n.thread_id == thread) {
            known.retain(|k| k.id != n.id);
            known.push(n.clone());
        }
        let already_applied = !targets.is_empty()
            && targets.iter().all(|t| {
                known.iter().any(|n| {
                    n.id == t.id && n.status_report_id.as_deref() == Some(&report.request_id)
                })
            });
        let update = if already_applied {
            Ok(Vec::new())
        } else if newer {
            Err("A newer status request superseded this response".into())
        } else {
            patches(&report, thread, &targets, &known)
        };
        let mut outcome = receipt.clone();
        outcome["request_id"] = json!(report.request_id);
        match update {
            Ok(patches) => {
                if !patches.is_empty() {
                    let mut next = stored.clone();
                    for n in patches {
                        next.retain(|old| old.id != n.id || old.thread_id != n.thread_id);
                        next.push(n);
                    }
                    let temp = app.file.with_extension("tmp");
                    tokio::fs::write(&temp, serde_json::to_vec_pretty(&next).map_err(api_error)?)
                        .await
                        .map_err(api_error)?;
                    tokio::fs::rename(temp, &app.file)
                        .await
                        .map_err(api_error)?;
                    *stored = next;
                    let _ = app.updates.send(now());
                }
                outcome["status"] = json!("applied");
                outcome["message"] = json!(
                    "Reported status, blockers and next step updated. Verification remains separate."
                );
            }
            Err(reason) => {
                outcome["status"] = json!(if newer { "superseded" } else { "rejected" });
                outcome["message"] = json!(reason);
            }
        }
        drop(stored);
        if matches!(text(&outcome, "status"), "applied" | "rejected")
            && receipt["transport"] == "fork"
        {
            let graph = app
                .flow
                .documents
                .lock()
                .await
                .get(thread)
                .map(|d| d.graph.clone());
            if let Some(graph) = graph {
                outcome["reviewed_input"] = crate::chat_actions::review_input(
                    text(&receipt, "goal"),
                    &graph,
                    &app.nodes.lock().await,
                );
                outcome["reviewed_history"] = receipt["input_history"].clone();
            }
        }
        receipts.insert(key, outcome.clone());
        chat_actions::save_receipts(app, &receipts).await?;
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn node() -> Node {
        Node {
            id: "n".into(),
            thread_id: "t".into(),
            parent_id: None,
            title: "R1".into(),
            status: "blocked".into(),
            lane: "aligned".into(),
            evidence: "Original evidence".into(),
            next: "Old next".into(),
            acceptance: "cargo test".into(),
            blockers: vec!["Old blocker".into()],
            reported_evidence: String::new(),
            status_report_id: None,
            hidden: false,
            revision: 0,
        }
    }
    fn report() -> StatusReport {
        StatusReport {
            version: 1,
            thread_id: "t".into(),
            request_id: "request".into(),
            updates: vec![Update {
                node_id: "n".into(),
                status: "complete".into(),
                blockers: vec![],
                next_step: "Verify deployment".into(),
                evidence: "Agent reports tests passed".into(),
            }],
        }
    }
    #[test]
    fn updates_only_reported_fields_and_rejects_stale_or_wrong_nodes() {
        let before = node();
        let mut r = report();
        let next = patches(
            &r,
            "t",
            std::slice::from_ref(&before),
            std::slice::from_ref(&before),
        )
        .unwrap()
        .remove(0);
        assert_eq!(next.status, "complete");
        assert!(next.blockers.is_empty());
        assert_eq!(next.next, "Verify deployment");
        assert_eq!(next.evidence, before.evidence);
        assert_eq!(next.acceptance, before.acceptance);
        assert_eq!(next.revision, 1);
        assert!(patches(&r, "t", std::slice::from_ref(&before), &[next]).is_err());
        r.updates[0].node_id = "other".into();
        assert!(
            patches(
                &r,
                "t",
                std::slice::from_ref(&before),
                std::slice::from_ref(&before)
            )
            .is_err()
        );
    }
    #[test]
    fn plain_prose_and_unsupported_fields_cannot_change_nodes() {
        assert!(parse("R1 is complete").is_err());
        assert!(parse("```goal-atlas-status\n{\"version\":1,\"thread_id\":\"t\",\"request_id\":\"r\",\"updates\":[],\"verified\":true}\n```").is_err());
        let mut r = report();
        r.updates[0].evidence.clear();
        assert!(patches(&r, "t", &[node()], &[node()]).is_err());
    }
    #[test]
    fn rejects_partial_duplicate_wrong_thread_and_invalid_status() {
        let mut r = report();
        assert!(patches(&r, "other", &[node()], &[node()]).is_err());
        r.updates.clear();
        assert!(patches(&r, "t", &[node()], &[node()]).is_err());
        r = report();
        r.updates[0].status = "verified".into();
        assert!(patches(&r, "t", &[node()], &[node()]).is_err());
        r.updates[0].status = "blocked".into();
        assert!(patches(&r, "t", &[node()], &[node()]).is_err());
        r = report();
        r.updates.push(Update {
            node_id: "n".into(),
            status: "pending".into(),
            blockers: vec![],
            next_step: String::new(),
            evidence: "Report".into(),
        });
        assert!(patches(&r, "t", &[node(), node()], &[node()]).is_err());
        assert!(parse("```goal-atlas-status\r\n{\"version\":1,\"thread_id\":\"t\",\"request_id\":\"r\",\"updates\":[]}\r\n```").is_ok());
    }
    #[tokio::test]
    async fn persists_once_and_does_not_change_goal_verification_or_identity() {
        let dir = std::env::temp_dir().join(format!("atlas-status-{}", now()));
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(windows)]
        let mut child = Command::new("cmd")
            .args(["/C", "more"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        #[cfg(not(windows))]
        let mut child = Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap()).lines();
        let client = Codex {
            _child: child,
            input,
            output,
            sequence: 0,
        };
        let original = node();
        let mut catalog = Catalog::default();
        catalog.goals.insert("t".into(), json!({"status":"paused"}));
        let receipts = BTreeMap::from([(
            "t:request".into(),
            json!({"action":"status","status":"queued","thread_id":"t","request_id":"request","targets":[original.clone()],"requested_at":1}),
        )]);
        let (updates, _) = broadcast::channel(16);
        let app = Arc::new(App {
            trace: Arc::new(std::sync::Mutex::new(trace::Cache::default())),
            flow: flow::Hub::load(dir.join("flows.json")).await.unwrap(),
            codex: Mutex::new(client),
            catalog: RwLock::new(catalog),
            nodes: Mutex::new(vec![original.clone()]),
            message_receipts: Mutex::new(receipts),
            reports: RwLock::new(BTreeMap::new()),
            codex_home: dir.clone(),
            file: dir.join("nodes.json"),
            updates,
        });
        let response = json!({"version":1,"thread_id":"t","request_id":"request","updates":[{"node_id":"n","status":"complete","blockers":[],"next_step":"Verify deployment","evidence":"Reported tests pass"}]});
        let rows = vec![
            json!({"item":{"id":"reply","type":"agentMessage","text":format!("```goal-atlas-status\n{response}\n```")}}),
        ];
        let queued = app.message_receipts.lock().await["t:request"].clone();
        app.message_receipts.lock().await.insert("t:newer".into(), json!({"action":"status","status":"queued","thread_id":"t","requested_at":2,"targets":[original.clone()]}));
        assert_eq!(
            apply(&app, "t", &rows).await.unwrap()[0]["status"],
            "superseded"
        );
        assert_eq!(app.nodes.lock().await[0].revision, 0);
        assert!(!app.file.exists());
        {
            let mut receipts = app.message_receipts.lock().await;
            receipts.remove("t:newer");
            receipts.insert("t:request".into(), queued);
        }
        assert_eq!(
            apply(&app, "t", &rows).await.unwrap()[0]["status"],
            "applied"
        );
        let saved: Vec<Node> = serde_json::from_slice(&std::fs::read(&app.file).unwrap()).unwrap();
        assert_eq!(saved[0].revision, 1);
        assert_eq!(saved[0].status, "complete");
        assert!(saved[0].blockers.is_empty());
        assert_eq!(saved[0].acceptance, original.acceptance);
        assert_eq!(saved[0].evidence, original.evidence);
        assert_eq!(app.catalog.read().await.goals["t"]["status"], "paused");
        apply(&app, "t", &rows).await.unwrap();
        assert_eq!(app.nodes.lock().await[0].revision, 1);
        app.catalog
            .write()
            .await
            .threads
            .insert("t".into(), json!({"id":"t"}));
        let mut headers = HeaderMap::new();
        headers.insert("x-goal-atlas", "1".parse().unwrap());
        let request = |revision, hidden, confirmed| VisibilityRequest {
            thread_id: "t".into(),
            node_id: "n".into(),
            revision,
            hidden,
            confirmed,
        };
        assert_eq!(
            node_visibility(
                State(app.clone()),
                headers.clone(),
                Json(request(1, true, false))
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            node_visibility(
                State(app.clone()),
                headers.clone(),
                Json(request(0, true, true))
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        let hidden = node_visibility(
            State(app.clone()),
            headers.clone(),
            Json(request(1, true, true)),
        )
        .await
        .unwrap()
        .0;
        assert!(hidden.hidden);
        assert_eq!(hidden.evidence, saved[0].evidence);
        assert_eq!(hidden.status, saved[0].status);
        assert_eq!(hidden.next, saved[0].next);
        let persisted: Vec<Node> =
            serde_json::from_slice(&std::fs::read(&app.file).unwrap()).unwrap();
        assert!(persisted[0].hidden);
        assert_eq!(persisted.len(), 1);
        let mut agent = hidden.clone();
        agent.hidden = false;
        let reported = persist_node(app.clone(), headers.clone(), agent, true)
            .await
            .unwrap()
            .0;
        assert!(reported.hidden, "Agent reports must not unhide a node");
        let restored = node_visibility(
            State(app.clone()),
            headers,
            Json(request(reported.revision, false, false)),
        )
        .await
        .unwrap()
        .0;
        assert!(!restored.hidden);
        assert_eq!(app.catalog.read().await.goals["t"]["status"], "paused");
        let mut other = restored.clone();
        other.id = "other".into();
        app.nodes.lock().await.push(other.clone());
        let bulk_request = |second_revision| {
            vec![
                VisibilityRequest {
                    thread_id: "t".into(),
                    node_id: "n".into(),
                    revision: restored.revision,
                    hidden: true,
                    confirmed: true,
                },
                VisibilityRequest {
                    thread_id: "t".into(),
                    node_id: "other".into(),
                    revision: second_revision,
                    hidden: true,
                    confirmed: true,
                },
            ]
        };
        assert_eq!(
            bulk_node_visibility(
                State(app.clone()),
                headers_for_test(),
                Json(bulk_request(0))
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        assert!(
            app.nodes.lock().await.iter().all(|n| !n.hidden),
            "Failed batch must change nothing"
        );
        let hidden = bulk_node_visibility(
            State(app.clone()),
            headers_for_test(),
            Json(bulk_request(other.revision)),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(hidden.len(), 2);
        let persisted: Vec<Node> =
            serde_json::from_slice(&std::fs::read(&app.file).unwrap()).unwrap();
        assert!(persisted.iter().all(|n| n.hidden));
        assert!(
            persisted
                .iter()
                .all(|n| n.evidence == original.evidence && n.status == restored.status)
        );
        app.codex.lock().await._child.kill().await.unwrap();
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }
    fn headers_for_test() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-goal-atlas", "1".parse().unwrap());
        headers
    }
}
