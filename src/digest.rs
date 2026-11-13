//! Extract recorded facts without a model. Cached contributions are invalidated by exact input.
use crate::flow::{Component, Edge, Evidence, Graph};
use crate::{reported_steps, text};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Default, Serialize, Deserialize)]
pub struct Cache {
    entries: BTreeMap<String, Entry>,
}
impl Cache {
    pub fn review_revision(&self) -> String {
        use std::hash::{Hash, Hasher};
        let records: Vec<(&str, Value)> = self
            .entries
            .iter()
            .filter_map(|(id, entry)| {
                serde_json::from_str(entry.input.strip_prefix("scope-v2:")?)
                    .ok()
                    .map(|record| (id.as_str(), record))
            })
            .collect();
        let review_turns: std::collections::BTreeSet<_> = records
            .iter()
            .filter(|(_, r)| r["review_request"] == true)
            .filter_map(|(_, r)| r["turn_id"].as_str())
            .collect();
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        for (id, record) in &records {
            if record["review_request"] == true
                || record["turn_id"]
                    .as_str()
                    .is_some_and(|turn| review_turns.contains(turn))
            {
                continue;
            }
            id.hash(&mut hash);
            record.to_string().hash(&mut hash);
        }
        format!("{:016x}", hash.finish())
    }
}
#[derive(Serialize, Deserialize)]
struct Entry {
    input: String,
    facts: Vec<Component>,
}

fn clip(s: &str, limit: usize) -> String {
    s.chars()
        .scan(0, |bytes, c| {
            *bytes += c.len_utf8();
            (*bytes <= limit).then_some(c)
        })
        .collect()
}

/// Quotes always retain an exact substring; labels and prose are explicitly extraction, not verification.
fn fact(id: String, title: String, kind: &str, lane: &str, source: &str, quote: &str) -> Component {
    Component {
        id,
        title: if title.trim().is_empty() {
            "Recorded item".into()
        } else {
            clip(&title, 180)
        },
        kind: kind.into(),
        lane: lane.into(),
        status: "unknown".into(),
        description: "Extracted from recorded history; not independently verified.".into(),
        next: "Review the source and clarify relationships if needed.".into(),
        evidence: vec![Evidence {
            source_id: source.into(),
            quote: clip(quote, 600),
        }],
    }
}

/// Preserve explicit reports and changed paths; never guess dependencies or completion from command success.
fn extract(record: &Value) -> Vec<Component> {
    if record["review_request"] == true {
        return Vec::new();
    }
    let source = text(record, "source_id");
    let mut facts = Vec::new();
    if text(record, "type") == "agentMessage" {
        for report in reported_steps(&[record.clone()], "") {
            let line = text(record, "text")
                .lines()
                .find(|line| line.contains(&report.title))
                .unwrap_or("");
            if line.is_empty() {
                continue;
            }
            let mut n = fact(
                report.id,
                report.title,
                "Reported step",
                &report.lane,
                source,
                line,
            );
            n.id = format!("{}-{}", source.split(':').next().unwrap_or("report"), n.id);
            n.status = report.status;
            n.description = clip(&report.evidence, 2000);
            n.next = clip(&report.next, 2000);
            facts.push(n);
        }
    }
    if text(record, "type") == "userMessage" {
        let content = crate::flow::searchable(&record["content"]);
        if content.len() >= 30
            && !content.trim_start().starts_with('<')
            && !content.starts_with("# AGENTS.md")
        {
            facts.push(fact(
                format!("request-{source}"),
                clip(content.lines().next().unwrap_or(&content), 100),
                "Later request",
                "aligned",
                source,
                &content,
            ));
        }
    }
    for change in record["changes"].as_array().into_iter().flatten() {
        let path = text(change, "path");
        if path.is_empty() {
            continue;
        }
        // Group exact recorded directories, without claiming that a changed file is a completed subsystem.
        let normalized = path.replace('\\', "/");
        let group = normalized
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .unwrap_or("Project files");
        facts.push(fact(
            format!("files-{group}"),
            group.into(),
            "Changed files",
            "addition",
            source,
            path,
        ));
    }
    facts
}

/// Reuse unchanged contributions, discard removed records, and compose a bounded, source-linked outline.
pub fn build(
    id: &str,
    goal: &Value,
    pages: &[(String, Vec<Value>)],
    previous: Cache,
) -> (Graph, Cache, usize, usize) {
    let mut entries = previous.entries;
    let mut next = BTreeMap::new();
    let mut reports = BTreeMap::new();
    let mut paths = BTreeMap::new();
    let mut requests = Vec::new();
    let mut original = None;
    let mut changed = 0;
    let mut reused = 0;
    let mut total = 0;
    for (_, records) in pages {
        for record in records {
            total += 1;
            let source = text(record, "source_id");
            if original.is_none() && text(record, "type") == "userMessage" && source.starts_with(id)
            {
                let content = crate::flow::searchable(&record["content"]);
                if !content.trim().is_empty() {
                    original = Some((source.to_owned(), content));
                }
            }
            let input = format!("scope-v2:{record}");
            let entry = match entries.remove(source) {
                Some(entry) if entry.input == input => {
                    reused += 1;
                    entry
                }
                _ => {
                    changed += 1;
                    Entry {
                        input,
                        facts: extract(record),
                    }
                }
            };
            for n in &entry.facts {
                match n.kind.as_str() {
                    "Reported step" => {
                        reports.insert(n.id.clone(), n.clone());
                    }
                    "Changed files" => {
                        paths.insert(n.id.clone(), n.clone());
                    }
                    _ => requests.push(n.clone()),
                }
            }
            next.insert(source.into(), entry);
        }
    }
    let (source, quote) =
        if let Some(objective) = goal["objective"].as_str().filter(|s| !s.trim().is_empty()) {
            (format!("goal:{id}"), objective.to_owned())
        } else {
            original.unwrap_or_else(|| (format!("goal:{id}"), crate::flow::searchable(goal)))
        };
    requests.retain(|n| n.evidence[0].source_id != source);
    let mut root = fact(
        "original-goal".into(),
        "Original goal".into(),
        "goal",
        "aligned",
        &source,
        &quote,
    );
    root.description = clip(&quote, 10000);
    let omitted = reports.len().saturating_sub(32)
        + paths.len().saturating_sub(16)
        + requests.len().saturating_sub(12);
    let mut nodes = vec![root];
    nodes.extend(reports.into_values().take(32));
    // Stable IDs contain no arbitrary path length or characters; the original directory remains in the label.
    for (i, mut n) in paths.into_values().take(16).enumerate() {
        n.id = format!("changed-files-{i}");
        nodes.push(n);
    }
    for (i, mut n) in requests.into_iter().rev().take(12).enumerate() {
        n.id = format!("later-request-{i}");
        nodes.push(n);
    }
    let edges = nodes
        .iter()
        .skip(1)
        .map(|n| Edge {
            source: "original-goal".into(),
            target: n.id.clone(),
            label: if n.kind == "Reported step" {
                "Reported work"
            } else {
                "Recorded in history"
            }
            .into(),
            feedback: false,
        })
        .collect();
    let graph = Graph {summary:format!("Local history outline: {total} records scanned, {changed} new or changed, {reused} reused. No AI tokens used."),nodes,edges,inspected_files:pages.iter().map(|(file,_)|file.clone()).collect(),limitations:vec![
        "Local extraction is an evidence index, not a complete semantic reconstruction. Connections show recorded association, not inferred execution dependencies. Status labels are agent reports, not verified outcomes.".into(),
        format!("Human messages are attributed to the user, not AI drift. Changed directories remain unclassified until reviewed. {omitted} additional extracted items are outside this bounded overview. All input records were scanned; unstructured prose and command output remain in the source snapshot. Optional AI review receives only this digest and cannot recover omitted meaning."),
    ]};
    (graph, Cache { entries: next }, changed, reused)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_revision_ignores_review_turns_but_tracks_new_evidence() {
        let record = json!({"source_id":"t:work","type":"command","turn_id":"work","output_excerpt":"check passed"});
        let (_, cache, _, _) = build(
            "t",
            &json!({"objective":"Build"}),
            &[("history".into(), vec![record.clone()])],
            Cache::default(),
        );
        let revision = cache.review_revision();
        let (_, cache, _, _) = build(
            "t",
            &json!({"objective":"Build"}),
            &[(
                "history".into(),
                vec![
                    record.clone(),
                    json!({"source_id":"t:request","type":"userMessage","turn_id":"review","review_request":true}),
                    json!({"source_id":"t:reply","type":"agentMessage","turn_id":"review","text":"Review complete"}),
                ],
            )],
            cache,
        );
        assert_eq!(revision, cache.review_revision());
        let mut changed = record;
        changed["output_excerpt"] = json!("check failed");
        let (_, cache, _, _) = build(
            "t",
            &json!({"objective":"Build"}),
            &[("history".into(), vec![changed])],
            cache,
        );
        assert_ne!(revision, cache.review_revision());
    }
    use serde_json::json;
    #[test]
    fn human_requests_are_not_drift_and_file_changes_stay_unclassified() {
        assert!(extract(&json!({"review_request":true,"source_id":"t:review","type":"userMessage","content":[{"text":"Please review the existing goal and scope."}]})).is_empty());
        let human = extract(
            &json!({"source_id":"t:u2","type":"userMessage","content":[{"text":"Please add an export option to this project as well."}]}),
        );
        assert_eq!(human[0].kind, "Later request");
        assert_eq!(human[0].lane, "aligned");
        let file = extract(
            &json!({"source_id":"t:f","type":"fileChange","changes":[{"path":"src/new.rs"}]}),
        );
        assert_eq!(file[0].lane, "addition");
        assert_ne!(file[0].lane, "drift");
        let drift = extract(
            &json!({"source_id":"t:a","type":"agentMessage","text":"Scope check:\n| R2 Unrequested work | DRIFT | Reported evidence | Return to scope |"}),
        );
        assert_eq!(drift[0].lane, "drift");
    }
    #[test]
    fn goal_added_mid_chat_is_the_scope_anchor() {
        let pages = vec![(
            "history".into(),
            vec![
                serde_json::json!({"source_id":"t:first","type":"userMessage","content":[{"text":"An earlier unrelated conversation"}]}),
            ],
        )];
        let (graph, _, _, _) = build(
            "t",
            &serde_json::json!({"objective":"Build the requested system"}),
            &pages,
            Cache::default(),
        );
        assert_eq!(graph.nodes[0].description, "Build the requested system");
        assert_eq!(graph.nodes[0].evidence[0].source_id, "goal:t");
    }
    #[test]
    fn incremental_extraction_preserves_reports_and_invalidates_changed_or_removed_records() {
        let mut pages = vec![(
            "h.json".into(),
            vec![
                json!({"source_id":"t:u","type":"userMessage","content":[{"text":"Build the original project and preserve the goal"}]}),
                json!({"source_id":"t:a","type":"agentMessage","text":"Scope check:\n| Requirement | Status | Evidence | Cause / correction |\n| R1 Service | ON TRACK | Implemented source | Run tests |"}),
            ],
        )];
        let (g, c, n, r) = build("t", &json!({}), &pages, Cache::default());
        assert_eq!((n, r), (2, 0));
        assert_eq!(g.nodes[1].status, "in_progress");
        let (_, c, n, r) = build("t", &json!({}), &pages, c);
        assert_eq!((n, r), (0, 2));
        pages[0].1[1]["text"] =
            json!("Scope check:\n| R1 Service | BLOCKED | Failed check | Fix error |");
        let (g, c, n, r) = build("t", &json!({}), &pages, c);
        assert_eq!((n, r), (1, 1));
        assert_eq!(g.nodes[1].status, "blocked");
        pages[0].1.pop();
        let (g, _, _, _) = build("t", &json!({}), &pages, c);
        assert_eq!(g.nodes.len(), 1);
    }
}
