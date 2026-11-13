//! Original-chat scope replies and bounded validation of their cited files.
use crate::flow::Graph;
use crate::*;
use std::collections::BTreeSet;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    version: u8,
    thread_id: String,
    request_id: String,
    graph: Graph,
    #[serde(default)]
    added_work_summary: Option<AddedWorkSummary>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AddedWorkSummary {
    text: String,
    evidence: Vec<flow::Evidence>,
}

fn parse(text: &str) -> Result<Reply, String> {
    let text = text.replace("\r\n", "\n");
    if text.matches("```goal-layers-scope\n").count() != 1 {
        return Err("Expected one scope response block".into());
    }
    let body = text
        .split_once("```goal-layers-scope\n")
        .unwrap()
        .1
        .split_once("```")
        .ok_or("Incomplete scope response")?
        .0;
    if body.len() > 128000 {
        return Err("Scope response too large".into());
    }
    serde_json::from_str(body).map_err(|_| "Invalid scope response".into())
}

pub fn instruction(
    thread: &str,
    request: &str,
    goal: &str,
    graph: &Graph,
) -> Result<String, ApiError> {
    let example = json!({"version":1,"thread_id":thread,"request_id":request,"added_work_summary":{"text":"Explain added work, why it was added, authorization and uncertainty.","evidence":[]},"graph":{"summary":"Scope breakdown","nodes":[],"edges":[],"inspected_files":[],"limitations":[]}});
    let context = json!({"summary":graph.summary,"nodes":graph.nodes.iter().map(|n|json!({"id":n.id,"title":n.title,"kind":n.kind,"lane":n.lane,"status":n.status,"description":n.description.chars().take(240).collect::<String>(),"summary_citations":n.evidence.iter().take(1).map(|e|json!({"source_id":e.source_id,"quote":e.quote.chars().take(240).collect::<String>()})).collect::<Vec<_>>()})).collect::<Vec<_>>(),"edges":graph.edges});
    Ok(format!(
        "In THIS existing chat, reread the full original goal and all referenced files that the goal asks you to read. Analyze only; do not implement or change files. Break the original scope into detailed nested nodes without duplicating current work. Preserve human instructions and the existing goal. Return exactly one fenced block tagged goal-layers-scope with this envelope: {example}. Graph nodes must have id, title, kind (Scope detail), lane (aligned), status (unknown, pending, in_progress, blocked, or complete), description, next, and evidence (source_id and quote). For every new node, report its current status from this chat and the referenced files, not just the scope structure. Explain blockers in description and the next action in next. Use unknown when the evidence cannot establish status. A complete status is only reported completion and must cite supporting evidence beyond the goal quotation. Use new unique IDs. Every new node must quote goal:{thread} exactly to establish original scope. Include file names and findings in descriptions, and list files actually read in inspected_files. State missing/unreadable files in limitations; never claim they were read. Edges need source, target, label, feedback:false; attach every new node to one existing required-work node or new child. Do not return existing nodes. Combined graph limit: 80 nodes and 160 edges. If no details are missing, return empty nodes/edges. Always return added_work_summary about ALL EXISTING nodes whose lane is addition or drift, not merely new nodes created during this review. Explain what work was added, why, whether the user authorized it and what remains uncertain. When such nodes exist, evidence must NOT be empty; copy at least one supplied summary_citation exactly. Cite the supplied summary_citations using exact source_id and quote. Do not label ordinary implementation prerequisites as drift. If no added work exists, say so with empty evidence. Do not claim independent verification. Do not mark work complete merely because you read or planned it. Keep the response in this same chat.\nOriginal goal (source goal:{thread}):\n{goal}\nExisting graph:\n{}",
        serde_json::to_string(&context).map_err(api_error)?
    ))
}

fn expected(
    reply: &Reply,
    thread: &str,
    receipt: &Value,
    file_sources: &BTreeMap<String, String>,
) -> Result<Graph, String> {
    if reply.version != 1
        || reply.thread_id != thread
        || receipt["thread_id"] != thread
        || receipt["request_id"] != reply.request_id
    {
        return Err("Scope reply does not match its original chat/request".into());
    }
    let original: Graph = serde_json::from_value(receipt["baseline_graph"].clone())
        .map_err(|_| "Missing original graph")?;
    let mut sources = BTreeMap::new();
    sources.extend(file_sources.clone());
    sources.insert(format!("goal:{thread}"), text(receipt, "goal").to_owned());
    for n in &original.nodes {
        for e in &n.evidence {
            sources
                .entry(e.source_id.clone())
                .or_insert_with(String::new)
                .push_str(&format!("\n{}", e.quote));
        }
    }
    if receipt["review_version"] == 2 {
        let summary = reply
            .added_work_summary
            .as_ref()
            .ok_or("Missing Added Work summary")?;
        if summary.text.trim().is_empty()
            || summary.text.len() > 12000
            || summary.evidence.len() > 40
        {
            return Err("Invalid Added Work summary".into());
        }
        if original.nodes.iter().any(|n| n.lane != "aligned") && summary.evidence.is_empty() {
            return Err("Added Work summary requires cited evidence".into());
        }
        for evidence in &summary.evidence {
            if evidence.quote.trim().is_empty()
                || evidence.quote.len() > 4000
                || !sources
                    .get(&evidence.source_id)
                    .is_some_and(|source| source.contains(&evidence.quote))
            {
                return Err("Added Work summary has an unsupported citation".into());
            }
        }
    }
    let mut graph = merge(&original, reply.graph.clone(), &format!("goal:{thread}"))
        .map_err(|e| e.to_string())?;
    graph.limitations.push("Scope breakdown returned by the original Codex chat. File inspection is reported by that chat, not independently verified by Goal Layers.".into());
    let files = graph.inspected_files.iter().cloned().collect();
    crate::flow::validate(&graph, &sources, &files).map_err(|e| e.to_string())?;
    Ok(graph)
}

async fn cited_files(graph: &Graph, cwd: &str) -> Result<BTreeMap<String, String>, String> {
    let ids: BTreeSet<_> = graph
        .nodes
        .iter()
        .flat_map(|n| &n.evidence)
        .filter_map(|e| {
            e.source_id
                .strip_prefix("file:")
                .map(|path| (e.source_id.clone(), path.to_owned()))
        })
        .collect();
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    if ids.len() > 16 {
        return Err("Too many cited files for scope verification".into());
    }
    let root = tokio::fs::canonicalize(cwd)
        .await
        .map_err(|_| "Project directory unavailable for citation verification")?;
    let mut sources = BTreeMap::new();
    let mut total = 0;
    for (id, path) in ids {
        if path.len() > 1024 {
            return Err("Invalid citation path".into());
        }
        let path = tokio::fs::canonicalize(root.join(path))
            .await
            .map_err(|_| "Cited file unavailable")?;
        if !path.starts_with(&root) {
            return Err("Cited file is outside this project".into());
        }
        let mut listed = false;
        for entry in graph.inspected_files.iter().take(100) {
            if tokio::fs::canonicalize(root.join(entry))
                .await
                .ok()
                .as_ref()
                == Some(&path)
            {
                listed = true;
                break;
            }
        }
        if !listed {
            return Err("Cited file was not listed as inspected".into());
        }
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|_| "Cannot read cited file")?;
        let mut bytes = Vec::new();
        file.take(1_048_577)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "Cannot read cited file")?;
        total += bytes.len();
        if bytes.len() > 1_048_576 || total > 8_388_608 {
            return Err("Cited files exceed verification size limit".into());
        }
        sources.insert(
            id,
            String::from_utf8(bytes).map_err(|_| "Cited file is not UTF-8 text")?,
        );
    }
    Ok(sources)
}

pub fn is_request(item: &Value, request: &str) -> bool {
    !request.is_empty() && text(item, "type") == "userMessage" && item.to_string().contains(request)
}

pub(super) fn project_directory(catalog: &Catalog, thread: &str) -> Option<String> {
    let t = catalog.threads.get(thread)?;
    let cwd = text(t, "cwd");
    if std::path::Path::new(cwd).is_dir() {
        return Some(cwd.into());
    }
    let id = project_for(t, &catalog.projects)?;
    let project = catalog.projects.iter().find(|p| p["id"] == id)?;
    let roots = project["roots"].as_array()?;
    if roots.len() != 1 {
        return None;
    }
    roots[0]["path"].as_str().map(str::to_owned)
}

fn attachments_unchanged(original: &Graph, current: &Graph, additions: &Graph) -> bool {
    let mut pending: Vec<_> = additions
        .edges
        .iter()
        .filter(|e| original.nodes.iter().any(|n| n.id == e.source))
        .map(|e| e.source.as_str())
        .collect();
    let mut seen = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let old = original.nodes.iter().find(|n| n.id == id);
        let new = current.nodes.iter().find(|n| n.id == id);
        if old.is_none()
            || new.is_none()
            || serde_json::to_value(old).ok() != serde_json::to_value(new).ok()
        {
            return false;
        }
        let old_edges: Vec<_> = original.edges.iter().filter(|e| e.target == id).collect();
        let new_edges: Vec<_> = current.edges.iter().filter(|e| e.target == id).collect();
        if serde_json::to_value(&old_edges).ok() != serde_json::to_value(&new_edges).ok() {
            return false;
        }
        pending.extend(
            old_edges
                .into_iter()
                .filter(|e| !e.feedback)
                .map(|e| e.source.as_str()),
        );
    }
    true
}

pub async fn apply(app: &Arc<App>, thread: &str, rows: &[Value]) -> Result<Vec<Value>, ApiError> {
    let current_goal = app
        .catalog
        .read()
        .await
        .goals
        .get(thread)
        .and_then(|g| g["objective"].as_str())
        .map(str::to_owned);
    let cwd = project_directory(&*app.catalog.read().await, thread).unwrap_or_default();
    let mut receipts = app.message_receipts.lock().await;
    let mut outcomes = Vec::new();
    let mut changed = false;
    for receipt in receipts.values_mut().filter(|r| {
        r["thread_id"] == thread
            && r["action"] == "breakdown"
            && matches!(text(r, "status"), "queued" | "delivered")
    }) {
        if rows
            .iter()
            .any(|row| is_request(row.get("item").unwrap_or(row), text(receipt, "request_id")))
        {
            if receipt["status"] != "delivered" {
                receipt["status"] = json!("delivered");
                receipt["message"] = json!(
                    "Request received by the original Codex chat. Waiting for its completed scope breakdown."
                );
                changed = true;
            }
            outcomes.push(receipt.clone());
        }
    }
    if changed {
        crate::chat_actions::save_receipts(app, &receipts).await?;
    }
    for row in rows {
        let item = row.get("item").unwrap_or(row);
        if text(item, "type") != "agentMessage" {
            continue;
        }
        let Ok(reply) = parse(text(item, "text")) else {
            continue;
        };
        let key = format!("{thread}:{}", reply.request_id);
        let Some(receipt) = receipts.get(&key).cloned() else {
            continue;
        };
        if receipt["action"] != "breakdown" {
            continue;
        }
        let retry_sources = receipt["status"] == "rejected"
            && matches!(
                text(&receipt, "message"),
                "The agent cited an unknown source"
                    | "Project directory unavailable for citation verification"
                    | "Outline changed while waiting; request a fresh breakdown"
                    | "Added Work summary requires cited evidence"
                    | "Missing Added Work summary"
            );
        let restore_applied = receipt["status"] == "applied"
            && app
                .flow
                .documents
                .lock()
                .await
                .get(thread)
                .is_some_and(|d| {
                    reply
                        .graph
                        .nodes
                        .iter()
                        .all(|node| !d.graph.nodes.iter().any(|n| n.id == node.id))
                })
            && !reply.graph.nodes.is_empty();
        if !restore_applied
            && !retry_sources
            && matches!(
                text(&receipt, "status"),
                "applied" | "rejected" | "superseded"
            )
        {
            outcomes.push(receipt);
            continue;
        }
        // Wait for any local reconstruction to finish before checking the baseline.
        let Ok(_permit) = app.flow.limit.clone().try_acquire_owned() else {
            continue;
        };
        let newer = receipts.values().any(|r| {
            r["action"] == "breakdown"
                && r["thread_id"] == thread
                && r["requested_at"].as_u64() > receipt["requested_at"].as_u64()
        });
        let mut documents = app.flow.documents.lock().await;
        let mut outcome = receipt.clone();
        let file_sources = cited_files(&reply.graph, &cwd).await;
        let candidate = if current_goal.as_deref() != receipt["goal"].as_str() {
            Err("Goal changed while waiting; request a fresh breakdown".into())
        } else if newer {
            Err("A newer scope request superseded this reply".into())
        } else {
            match &file_sources {
                Ok(sources) => expected(&reply, thread, &receipt, &sources),
                Err(error) => Err(error.clone()),
            }
        };
        let candidate = candidate.and_then(|graph| {
            let current = documents
                .get(thread)
                .ok_or("Current outline is unavailable")?;
            let value = serde_json::to_value(&current.graph).map_err(|e| e.to_string())?;
            if value != receipt["baseline_graph"]
                && value != serde_json::to_value(&graph).map_err(|e| e.to_string())?
            {
                let original = serde_json::from_value(receipt["baseline_graph"].clone())
                    .map_err(|_| "Missing original graph")?;
                if !attachments_unchanged(&original, &current.graph, &reply.graph) {
                    return Err(
                        "Scope attachment changed while waiting; request a fresh breakdown".into(),
                    );
                }
                let mut rebased = receipt.clone();
                rebased["baseline_graph"] = value;
                let mut sources = file_sources.as_ref().map_err(Clone::clone)?.clone();
                // Retain the validated request-time citations when unrelated live nodes refresh.
                for node in &original.nodes {
                    for evidence in &node.evidence {
                        sources
                            .entry(evidence.source_id.clone())
                            .or_default()
                            .push_str(&format!("\n{}", evidence.quote));
                    }
                }
                return expected(&reply, thread, &rebased, &sources);
            }
            Ok(graph)
        });
        match candidate {
            Ok(graph) => {
                let current = documents.get(thread).unwrap();
                if serde_json::to_value(&current.graph).map_err(api_error)?
                    != serde_json::to_value(&graph).map_err(api_error)?
                {
                    let mut next = documents.clone();
                    let document = next.get_mut(thread).unwrap();
                    document.graph = graph;
                    document.mode = "chat".into();
                    document.generated_at = now();
                    let temp = app.flow.file.with_extension("tmp");
                    tokio::fs::write(&temp, serde_json::to_vec_pretty(&next).map_err(api_error)?)
                        .await
                        .map_err(api_error)?;
                    tokio::fs::rename(temp, &app.flow.file)
                        .await
                        .map_err(api_error)?;
                    *documents = next;
                    let _ = app.updates.send(now());
                }
                outcome["status"] = json!("applied");
                if let Some(summary) = &reply.added_work_summary {
                    outcome["added_work_summary"] = json!(summary);
                }
                outcome["message"] = json!(
                    "Original-chat scope breakdown added to the tree. New details remain unverified."
                );
                if let Some(status_id) = receipt["status_request_id"].as_str() {
                    let status = receipts.get(&format!("{thread}:{status_id}"));
                    match status.map(|s| text(s, "status")) {
                        Some("applied") => {
                            outcome["message"] =
                                json!("Scope breakdown and evidence-based status review updated.")
                        }
                        Some("rejected" | "superseded") => {
                            outcome["status"] = json!("rejected");
                            outcome["message"] = json!(format!(
                                "Scope updated, but status review was not applied: {}",
                                status
                                    .map(|s| text(s, "message"))
                                    .unwrap_or("Unknown reason")
                            ));
                        }
                        _ => {
                            outcome["status"] = json!("awaiting_status");
                            outcome["message"] =
                                json!("Scope updated. Waiting for the same chat's status review.");
                        }
                    }
                }
                if outcome["status"] == "applied" && receipt["review_version"] == 2 {
                    outcome["reviewed_history"] = receipt["input_history"].clone();
                    outcome["reviewed_input"] = crate::chat_actions::review_input(
                        text(&receipt, "goal"),
                        &documents[thread].graph,
                        &app.nodes.lock().await,
                    );
                    outcome["message"] = json!(
                        "Deep review complete: scope, statuses and Added Work summary updated."
                    );
                }
            }
            Err(message) => {
                outcome["status"] = json!(if newer { "superseded" } else { "rejected" });
                outcome["message"] = json!(message);
                if receipt["review_version"] == 2 {
                    if let Some(document) = documents.get(thread) {
                        outcome["reviewed_input"] = crate::chat_actions::review_input(
                            text(&receipt, "goal"),
                            &document.graph,
                            &app.nodes.lock().await,
                        );
                        outcome["reviewed_history"] = receipt["input_history"].clone();
                    }
                }
            }
        }
        drop(documents);
        receipts.insert(key, outcome.clone());
        crate::chat_actions::save_receipts(app, &receipts).await?;
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

pub fn merge(original: &Graph, additions: Graph, goal_source: &str) -> Result<Graph, Error> {
    let mut graph = original.clone();
    let existing: BTreeSet<_> = original.nodes.iter().map(|n| n.id.clone()).collect();
    let mut added = BTreeSet::new();
    for node in additions.nodes {
        if existing.contains(&node.id) || !added.insert(node.id.clone()) {
            return Err(
                "Scope breakdown must add unique new nodes, not replace existing work".into(),
            );
        }
        if node.kind != "Scope detail"
            || node.lane != "aligned"
            || !matches!(
                node.status.as_str(),
                "unknown" | "pending" | "in_progress" | "blocked" | "complete"
            )
        {
            return Err("Scope-breakdown nodes must be unverified original-scope details".into());
        }
        if !node.evidence.iter().any(|e| e.source_id == goal_source) {
            return Err("Every new scope node must quote the original goal".into());
        }
        if node.status == "complete" && !node.evidence.iter().any(|e| e.source_id != goal_source) {
            return Err(
                "Reported completion requires supporting evidence beyond the goal quotation".into(),
            );
        }
        graph.nodes.push(node);
    }
    let mut parents = BTreeMap::new();
    for edge in additions.edges {
        if !added.contains(&edge.target)
            || edge.feedback
            || !(existing.contains(&edge.source) || added.contains(&edge.source))
        {
            return Err(
                "Scope breakdown may only attach new child nodes to the existing tree".into(),
            );
        }
        if parents
            .insert(edge.target.clone(), edge.source.clone())
            .is_some()
        {
            return Err("A scope-breakdown node has multiple parents".into());
        }
        graph.edges.push(edge);
    }
    for id in &added {
        let mut current = id;
        let mut visited = BTreeSet::new();
        while !existing.contains(current) {
            if !visited.insert(current) {
                return Err("Scope breakdown returned a cycle".into());
            }
            current = parents
                .get(current)
                .ok_or("Scope breakdown returned a disconnected node")?;
        }
        if original
            .nodes
            .iter()
            .any(|n| &n.id == current && n.lane != "aligned")
        {
            return Err("Original-scope details cannot be attached to added work".into());
        }
    }
    graph.inspected_files = additions.inspected_files;
    graph.limitations.extend(additions.limitations);
    Ok(graph)
}

pub fn preserve(refreshed: &Graph, previous: &Graph, thread: &str) -> Result<Graph, Error> {
    let nodes: Vec<_> = previous
        .nodes
        .iter()
        .filter(|n| n.kind == "Scope detail")
        .cloned()
        .collect();
    if nodes.is_empty() {
        return Ok(refreshed.clone());
    }
    let ids: BTreeSet<_> = nodes.iter().map(|n| n.id.as_str()).collect();
    let edges = previous
        .edges
        .iter()
        .filter(|e| ids.contains(e.target.as_str()))
        .cloned()
        .collect();
    let additions=Graph {summary:String::new(),edges,nodes,inspected_files:refreshed.inspected_files.clone(),limitations:vec!["Retained the original-chat scope breakdown; this local refresh does not reverify its cited project files.".into()]};
    let result = merge(refreshed, additions, &format!("goal:{thread}"))?;
    if result.nodes.len() > 80 || result.edges.len() > 160 {
        return Err("Refresh would exceed the graph limit; previous tree preserved".into());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn graph() -> Graph {
        serde_json::from_value(json!({"summary":"Goal","nodes":[{"id":"root","title":"Goal","kind":"goal","lane":"aligned","status":"unknown","description":"","next":"","evidence":[{"source_id":"goal:t","quote":"Build a tool"}]}],"edges":[],"inspected_files":[],"limitations":[]})).unwrap()
    }
    fn reply() -> Reply {
        serde_json::from_value(json!({"version":1,"thread_id":"t","request_id":"r","graph":{"summary":"Details","nodes":[{"id":"detail","title":"Build component","kind":"Scope detail","lane":"aligned","status":"unknown","description":"Read specification","next":"","evidence":[{"source_id":"goal:t","quote":"Build a tool"}]}],"edges":[{"source":"root","target":"detail","label":"includes","feedback":false}],"inspected_files":["spec.md"],"limitations":[]}})).unwrap()
    }
    #[test]
    fn validates_matching_reply_and_preserves_original() {
        let original = graph();
        let receipt = json!({"thread_id":"t","request_id":"r","goal":"Build a tool","baseline_graph":original});
        let merged = expected(&reply(), "t", &receipt, &BTreeMap::new()).unwrap();
        assert_eq!(
            serde_json::to_value(&merged.nodes[0]).unwrap(),
            receipt["baseline_graph"]["nodes"][0]
        );
        assert_eq!(merged.nodes.len(), 2);
        assert_eq!(merged.nodes[1].status, "unknown");
        assert!(
            merged
                .limitations
                .last()
                .unwrap()
                .contains("not independently verified")
        );
        assert!(expected(&reply(), "other", &receipt, &BTreeMap::new()).is_err());
        let mut bad = reply();
        bad.graph.nodes[0].evidence[0].quote = "invented requirement".into();
        assert!(expected(&bad, "t", &receipt, &BTreeMap::new()).is_err());
        let mut bad = reply();
        bad.graph.nodes[0].id = "root".into();
        assert!(expected(&bad, "t", &receipt, &BTreeMap::new()).is_err());
        let mut bad = reply();
        bad.graph.edges.clear();
        assert!(expected(&bad, "t", &receipt, &BTreeMap::new()).is_err());
        let mut bad = reply();
        bad.graph.nodes[0].status = "verified".into();
        assert!(expected(&bad, "t", &receipt, &BTreeMap::new()).is_err());
    }

    #[test]
    fn deep_review_requires_a_cited_added_work_summary() {
        let mut original = graph();
        let mut added = original.nodes[0].clone();
        added.id = "added".into();
        added.kind = "Changed files".into();
        added.lane = "addition".into();
        added.evidence = vec![flow::Evidence {
            source_id: "history:1".into(),
            quote: "Added export support".into(),
        }];
        original.nodes.push(added);
        let receipt = json!({"thread_id":"t","request_id":"r","goal":"Build a tool","baseline_graph":original,"review_version":2});
        let mut r = reply();
        assert!(expected(&r, "t", &receipt, &BTreeMap::new()).is_err());
        r.added_work_summary = Some(AddedWorkSummary {
            text: "Export work was added; authorization is uncertain.".into(),
            evidence: vec![flow::Evidence {
                source_id: "history:1".into(),
                quote: "Added export support".into(),
            }],
        });
        assert!(expected(&r, "t", &receipt, &BTreeMap::new()).is_ok());
        r.added_work_summary.as_mut().unwrap().evidence[0].quote = "Invented evidence".into();
        assert!(expected(&r, "t", &receipt, &BTreeMap::new()).is_err());
    }
    #[test]
    fn prompt_uses_original_chat() {
        let prompt = instruction("t", "r", "Build a tool", &graph()).unwrap();
        assert!(prompt.contains("THIS existing chat"));
        assert!(prompt.contains("all referenced files"));
        assert!(prompt.contains("report its current status"));
        assert!(parse("not a response").is_err());
    }

    #[test]
    fn breakdown_carries_reported_status_without_verifying_completion() {
        let mut original = graph();
        original.nodes[0].evidence.push(flow::Evidence {
            source_id: "history:check".into(),
            quote: "Tests passed".into(),
        });
        let receipt = json!({"thread_id":"t","request_id":"r","goal":"Build a tool","baseline_graph":original});
        for status in ["unknown", "pending", "in_progress", "blocked", "complete"] {
            let mut r = reply();
            r.graph.nodes[0].status = status.into();
            r.graph.nodes[0].next = "Review remaining checks".into();
            if status == "complete" {
                assert!(expected(&r, "t", &receipt, &BTreeMap::new()).is_err());
                r.graph.nodes[0].evidence.push(flow::Evidence {
                    source_id: "history:check".into(),
                    quote: "Tests passed".into(),
                });
            }
            let result = expected(&r, "t", &receipt, &BTreeMap::new()).unwrap();
            assert_eq!(result.nodes[1].status, status);
            assert_eq!(result.nodes[1].next, "Review remaining checks");
            assert!(
                result
                    .limitations
                    .last()
                    .unwrap()
                    .contains("not independently verified")
            );
            assert_eq!(
                preserve(&original, &result, "t").unwrap().nodes[1].status,
                status
            );
        }
    }

    #[test]
    fn live_changes_must_not_change_attachment_branches() {
        let mut original = graph();
        let mut other = original.nodes[0].clone();
        other.id = "other".into();
        original.nodes.push(other);
        let mut current = original.clone();
        current.summary = "Live summary".into();
        current.nodes[1].title = "Updated unrelated node".into();
        assert!(attachments_unchanged(&original, &current, &reply().graph));
        current.nodes[0].title = "Changed parent scope".into();
        assert!(!attachments_unchanged(&original, &current, &reply().graph));
    }

    #[test]
    fn local_refresh_preserves_nested_scope_and_rejects_missing_parent() {
        let previous = merge(&graph(), reply().graph, "goal:t").unwrap();
        let mut fresh = graph();
        fresh.nodes[0].status = "in_progress".into();
        let kept = preserve(&fresh, &previous, "t").unwrap();
        assert_eq!(kept.nodes.len(), 2);
        assert_eq!(kept.nodes[0].status, "in_progress");
        assert_eq!(
            serde_json::to_value(&kept.nodes[1]).unwrap(),
            serde_json::to_value(&previous.nodes[1]).unwrap()
        );
        let again = preserve(&fresh, &kept, "t").unwrap();
        assert_eq!(again.nodes.len(), 2);
        fresh.nodes[0].id = "replacement".into();
        assert!(preserve(&fresh, &previous, "t").is_err());
    }

    #[tokio::test]
    async fn file_citations_are_bounded_and_must_match_real_text() {
        let root = std::env::temp_dir().join(format!("scope-citations-{}", now()));
        std::fs::create_dir_all(root.join("project")).unwrap();
        std::fs::write(root.join("project/spec.md"), "Build a tool").unwrap();
        std::fs::write(root.join("outside.md"), "outside").unwrap();
        let mut r = reply();
        r.graph.inspected_files = vec!["spec.md".into()];
        r.graph.nodes[0].evidence.push(flow::Evidence {
            source_id: "file:spec.md".into(),
            quote: "Build a tool".into(),
        });
        let cwd = root.join("project");
        let mut catalog = Catalog::default();
        catalog.threads.insert(
            "t".into(),
            json!({"cwd":"missing-virtual-path","projectId":"p"}),
        );
        catalog
            .projects
            .push(json!({"id":"p","roots":[{"path":cwd}]}));
        assert_eq!(
            project_directory(&catalog, "t"),
            Some(cwd.to_string_lossy().into_owned())
        );
        let sources = cited_files(&r.graph, cwd.to_str().unwrap()).await.unwrap();
        let receipt = json!({"thread_id":"t","request_id":"r","goal":"Build a tool","baseline_graph":graph()});
        assert!(expected(&r, "t", &receipt, &sources).is_ok());
        r.graph.nodes[0].evidence[1].quote = "invented text".into();
        assert!(expected(&r, "t", &receipt, &sources).is_err());
        r.graph.nodes[0].evidence[1].source_id = "file:../outside.md".into();
        r.graph.inspected_files = vec!["../outside.md".into()];
        assert!(cited_files(&r.graph, cwd.to_str().unwrap()).await.is_err());
        r.graph.nodes[0].evidence[1].source_id = "file:spec.md".into();
        r.graph.inspected_files.clear();
        assert!(cited_files(&r.graph, cwd.to_str().unwrap()).await.is_err());
        r.graph.inspected_files = vec!["spec.md".into()];
        std::fs::write(root.join("project/spec.md"), vec![b'x'; 1_048_577]).unwrap();
        assert!(cited_files(&r.graph, cwd.to_str().unwrap()).await.is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn persists_once_and_rejects_changed_outline() {
        let dir = std::env::temp_dir().join(format!("scope-reply-{}", now()));
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
        let client = Codex {
            input: child.stdin.take().unwrap(),
            output: BufReader::new(child.stdout.take().unwrap()).lines(),
            _child: child,
            sequence: 0,
        };
        let mut catalog = Catalog::default();
        catalog
            .goals
            .insert("t".into(), json!({"objective":"Build a tool"}));
        let receipt = json!({"action":"breakdown","status":"queued","thread_id":"t","request_id":"r","requested_at":1,"goal":"Build a tool","baseline_graph":graph()});
        let (updates, _) = broadcast::channel(16);
        let app = Arc::new(App {
            trace: Arc::new(std::sync::Mutex::new(trace::Cache::default())),
            flow: flow::Hub::load(dir.join("flows.json")).await.unwrap(),
            codex: Mutex::new(client),
            catalog: RwLock::new(catalog),
            nodes: Mutex::new(vec![]),
            message_receipts: Mutex::new(BTreeMap::from([("t:r".into(), receipt.clone())])),
            reports: RwLock::new(BTreeMap::new()),
            codex_home: dir.clone(),
            file: dir.join("nodes.json"),
            updates,
        });
        app.flow.documents.lock().await.insert(
            "t".into(),
            flow::Document {
                thread_id: "t".into(),
                generated_at: 0,
                source_count: 1,
                source_threads: 1,
                graph: graph(),
                mode: "local".into(),
            },
        );
        let body = json!({"version":1,"thread_id":"t","request_id":"r","graph":reply().graph});
        let rows = vec![
            json!({"item":{"type":"agentMessage","text":format!("```goal-layers-scope\n{body}\n```")}}),
        ];
        let permit = app.flow.limit.clone().acquire_owned().await.unwrap();
        assert!(apply(&app, "t", &rows).await.unwrap().is_empty());
        drop(permit);
        app.flow
            .documents
            .lock()
            .await
            .get_mut("t")
            .unwrap()
            .graph
            .nodes[0]
            .title = "Newer scope".into();
        assert_eq!(
            apply(&app, "t", &rows).await.unwrap()[0]["status"],
            "rejected"
        );
        assert!(!app.flow.file.exists());
        app.flow.documents.lock().await.get_mut("t").unwrap().graph = graph();
        app.flow
            .documents
            .lock()
            .await
            .get_mut("t")
            .unwrap()
            .graph
            .summary = "Live summary".into();
        app.message_receipts
            .lock()
            .await
            .insert("t:r".into(), receipt);
        let delivered_rows = vec![
            json!({"item":{"type":"userMessage","content":[{"type":"inputText","text":"scope request r"}]}}),
        ];
        assert_eq!(
            apply(&app, "t", &delivered_rows).await.unwrap()[0]["status"],
            "delivered"
        );
        assert_eq!(app.flow.documents.lock().await["t"].graph.nodes.len(), 1);
        assert_eq!(
            apply(&app, "t", &delivered_rows).await.unwrap()[0]["status"],
            "delivered"
        );
        assert_eq!(
            apply(&app, "t", &rows).await.unwrap()[0]["status"],
            "applied"
        );
        let saved = std::fs::read(&app.flow.file).unwrap();
        assert_eq!(
            apply(&app, "t", &rows).await.unwrap()[0]["status"],
            "applied"
        );
        assert_eq!(std::fs::read(&app.flow.file).unwrap(), saved);
        assert_eq!(app.flow.documents.lock().await["t"].graph.nodes.len(), 2);
        assert_eq!(
            app.flow.documents.lock().await["t"].graph.summary,
            "Live summary"
        );
        app.flow.documents.lock().await.get_mut("t").unwrap().graph = graph();
        assert_eq!(
            apply(&app, "t", &rows).await.unwrap()[0]["status"],
            "applied"
        );
        assert_eq!(app.flow.documents.lock().await["t"].graph.nodes.len(), 2);
        let mut parent = app.message_receipts.lock().await["t:r"].clone();
        parent["status"] = json!("queued");
        parent["status_request_id"] = json!("r-status");
        let target = app.flow.documents.lock().await["t"]
            .nodes()
            .into_iter()
            .find(|n| n.id == "detail")
            .unwrap();
        {
            let mut receipts = app.message_receipts.lock().await;
            receipts.insert("t:r".into(), parent);
            receipts.insert("t:r-status".into(),json!({"action":"status","status":"queued","thread_id":"t","request_id":"r-status","targets":[target],"requested_at":2}));
        }
        assert_eq!(
            apply(&app, "t", &rows).await.unwrap()[0]["status"],
            "awaiting_status"
        );
        let report = json!({"version":1,"thread_id":"t","request_id":"r-status","updates":[{"node_id":"detail","status":"complete","blockers":[],"next_step":"","evidence":"Recorded tests passed"}]});
        let status_rows = vec![
            json!({"item":{"type":"agentMessage","text":format!("```goal-atlas-status\n{report}\n```")}}),
        ];
        assert_eq!(
            crate::status_updates::apply(&app, "t", &status_rows)
                .await
                .unwrap()[0]["status"],
            "applied"
        );
        assert_eq!(
            apply(&app, "t", &rows).await.unwrap()[0]["status"],
            "applied"
        );
        assert_eq!(app.nodes.lock().await[0].status, "complete");
        let mut parent = app.message_receipts.lock().await["t:r"].clone();
        parent["status"] = json!("queued");
        parent["review_version"] = json!(2);
        parent["input_history"] = json!(
            crate::chat_actions::history_revision(&app, "t")
                .await
                .unwrap()
        );
        app.message_receipts
            .lock()
            .await
            .insert("t:r".into(), parent);
        let mut reviewed = reply();
        reviewed.added_work_summary = Some(AddedWorkSummary {
            text: "No added work is recorded.".into(),
            evidence: vec![],
        });
        let rows = vec![
            json!({"item":{"type":"agentMessage","text":format!("```goal-layers-scope\n{}\n```",serde_json::to_string(&reviewed).unwrap())}}),
        ];
        assert_eq!(
            apply(&app, "t", &rows).await.unwrap()[0]["status"],
            "applied"
        );
        let mut headers = HeaderMap::new();
        headers.insert("x-goal-atlas", "1".parse().unwrap());
        let request = || {
            serde_json::from_value(
                json!({"action":"breakdown","request_id":"fresh-client-request","node_ids":[]}),
            )
            .unwrap()
        };
        let reused = tokio::time::timeout(
            Duration::from_secs(2),
            crate::chat_actions::send(
                State(app.clone()),
                Path("t".into()),
                headers.clone(),
                Json(request()),
            ),
        )
        .await
        .unwrap()
        .unwrap()
        .0;
        assert_eq!(reused["request_id"], "r");
        assert_eq!(reused["reused"], true);
        assert_eq!(reused["status"], "applied");
        app.message_receipts.lock().await.get_mut("t:r").unwrap()["status"] = json!("rejected");
        let rejected = tokio::time::timeout(
            Duration::from_secs(2),
            crate::chat_actions::send(
                State(app.clone()),
                Path("t".into()),
                headers.clone(),
                Json(request()),
            ),
        )
        .await
        .unwrap()
        .unwrap()
        .0;
        assert_eq!(rejected["status"], "rejected");
        assert_eq!(rejected["reused"], true);
        assert_eq!(
            app.codex.lock().await.sequence,
            0,
            "A cached review must not contact Codex"
        );
        let baseline = app.message_receipts.lock().await["t:r"]["reviewed_input"].clone();
        app.nodes.lock().await[0].status = "blocked".into();
        assert_ne!(
            baseline,
            crate::chat_actions::review_input(
                "Build a tool",
                &app.flow.documents.lock().await["t"].graph,
                &app.nodes.lock().await
            )
        );
        app.message_receipts.lock().await.get_mut("t:r").unwrap()["status"] = json!("delivered");
        let pending = tokio::time::timeout(
            Duration::from_secs(2),
            crate::chat_actions::send(
                State(app.clone()),
                Path("t".into()),
                headers,
                Json(request()),
            ),
        )
        .await
        .unwrap()
        .unwrap()
        .0;
        assert_eq!(pending["request_id"], "r");
        assert_eq!(pending["reused"], true);
        assert_eq!(
            app.codex.lock().await.sequence,
            0,
            "A pending review must not be resent or resumed"
        );
        assert_eq!(app.flow.documents.lock().await["t"].graph.nodes.len(), 2);
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
