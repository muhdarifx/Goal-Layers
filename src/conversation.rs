use crate::*;

type Requests = BTreeMap<(u32, String), Value>;
fn requests() -> &'static std::sync::Mutex<Requests> {
    static REQUESTS: std::sync::OnceLock<std::sync::Mutex<Requests>> = std::sync::OnceLock::new();
    REQUESTS.get_or_init(Default::default)
}
pub fn capture(pid: Option<u32>, request: &Value) -> bool {
    let Some(pid) = pid else { return false };
    if !matches!(
        text(request, "method"),
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/tool/requestUserInput"
            | "item/permissions/requestApproval"
    ) || request["params"]["threadId"].as_str().is_none()
    {
        return false;
    }
    let mut pending = requests().lock().unwrap();
    if pending.len() >= 128 {
        return false;
    }
    pending.insert((pid, request["id"].to_string()), request.clone());
    true
}

pub struct Response {
    request_id: String,
    decision: Option<String>,
    answers: Option<BTreeMap<String, Vec<String>>>,
}
pub async fn respond(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    if headers.get("x-goal-atlas").and_then(|v| v.to_str().ok()) != Some("1") {
        return Err((StatusCode::FORBIDDEN, "Local action required".into()));
    }
    let body = Response {
        request_id: value["request_id"].as_str().unwrap_or("").to_owned(),
        decision: value["decision"].as_str().map(str::to_owned),
        answers: serde_json::from_value(value["answers"].clone()).ok(),
    };
    let mut client = app.codex.lock().await;
    let key = (client._child.id().unwrap_or(0), body.request_id);
    let request = requests()
        .lock()
        .unwrap()
        .get(&key)
        .cloned()
        .ok_or((StatusCode::CONFLICT, "Request is no longer pending".into()))?;
    if request["params"]["threadId"] != id {
        return Err((StatusCode::FORBIDDEN, "Wrong conversation".into()));
    }
    let result = if text(&request, "method") == "item/tool/requestUserInput" {
        let answers = body
            .answers
            .ok_or((StatusCode::BAD_REQUEST, "Answers required".into()))?;
        let mut result = serde_json::Map::new();
        for question in request["params"]["questions"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let q = text(question, "id");
            let answer = answers
                .get(q)
                .filter(|a| {
                    !a.is_empty() && a.iter().all(|s| !s.trim().is_empty() && s.len() <= 32000)
                })
                .ok_or((StatusCode::BAD_REQUEST, "Answer every question".into()))?;
            result.insert(q.into(), json!({"answers":answer}));
        }
        json!({"answers":result})
    } else {
        let decision = body
            .decision
            .as_deref()
            .filter(|d| matches!(*d, "accept" | "decline"))
            .ok_or((StatusCode::BAD_REQUEST, "Explicit decision required".into()))?;
        if text(&request, "method") == "item/permissions/requestApproval" {
            json!({"permissions":if decision=="accept" {request["params"]["permissions"].clone()} else {json!({})},"scope":"turn"})
        } else {
            json!({"decision":decision})
        }
    };
    client
        .input
        .write_all(format!("{}\n", json!({"id":request["id"],"result":result})).as_bytes())
        .await
        .map_err(api_error)?;
    requests().lock().unwrap().remove(&key);
    Ok(Json(json!({"status":"answered"})))
}

#[derive(Deserialize)]
pub struct ReplyRequest {
    #[serde(flatten)]
    pub request: crate::chat_actions::Request,
    pub message: String,
}

#[derive(Deserialize)]
pub struct Page {
    cursor: Option<String>,
}

pub async fn read(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Query(page): Query<Page>,
) -> Result<Json<Value>, ApiError> {
    if !app.catalog.read().await.threads.contains_key(&id) {
        return Err((StatusCode::NOT_FOUND, "Unknown chat".into()));
    }
    if page.cursor.as_ref().is_some_and(|c| c.len() > 4096) {
        return Err((StatusCode::BAD_REQUEST, "Invalid history cursor".into()));
    }
    let mut client = app.codex.lock().await;
    let mut cursor = page.cursor;
    let mut messages = Vec::new();
    for _ in 0..8 {
        let result = client
            .call(
                "thread/items/list",
                json!({
                    "threadId":id,"sortDirection":"desc","limit":100,"cursor":cursor
                }),
            )
            .await
            .map_err(api_error)?;
        let rows = result["data"]
            .as_array()
            .ok_or_else(|| api_error("No conversation history"))?;
        messages.extend(rows.iter().filter_map(|row| {
            let mut item = message(row)?;
            item["turnId"] = row["turnId"].clone();
            item["startedAtMs"] = row["startedAtMs"].clone();
            item["completedAtMs"] = row["completedAtMs"].clone();
            Some(item)
        }));
        cursor = result["nextCursor"].as_str().map(str::to_owned);
        if cursor.is_none() || messages.iter().any(|m| m["role"] == "user") {
            break;
        }
    }
    let pending: Vec<Value> = requests()
        .lock()
        .unwrap()
        .iter()
        .filter(|((pid, _), r)| Some(*pid) == client._child.id() && r["params"]["threadId"] == id)
        .map(|((_, key), r)| json!({"id":key,"method":r["method"],"params":r["params"]}))
        .collect();
    Ok(Json(
        json!({"messages":messages,"nextCursor":cursor,"requests":pending}),
    ))
}

fn message(row: &Value) -> Option<Value> {
    let item = row.get("item").unwrap_or(row);
    let role = match text(item, "type") {
        "userMessage" => "user",
        "agentMessage" => "assistant",
        "commandExecution" | "mcpToolCall" | "fileChange" => "tool",
        _ => return None,
    };
    let body = if role == "tool" {
        let title = match text(item, "type") {
            "commandExecution" => crate::flow::searchable(&item["command"]),
            "mcpToolCall" => format!("{} / {}", text(item, "server"), text(item, "tool")),
            _ => "File changes".into(),
        };
        return Some(
            json!({"id":item["id"],"role":role,"text":title,"status":item["status"],"output":item.get("aggregatedOutput").or_else(||item.get("output")).cloned().unwrap_or(Value::Null)}),
        );
    } else if role == "assistant" {
        text(item, "text").to_owned()
    } else {
        item["content"]
            .as_array()?
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    Some(
        json!({"id":item["id"],"role":role,"text":body,"questions":item.get("questions"),"phase":item.get("phase")}),
    )
}

pub async fn settings(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !app.catalog.read().await.threads.contains_key(&id) {
        return Err((StatusCode::NOT_FOUND, "Unknown chat".into()));
    }
    let mut client = app.codex.lock().await;
    let mut thread = client
        .call("thread/read", json!({"threadId":id,"includeTurns":false}))
        .await
        .map_err(api_error)?;
    let turns = client
        .call(
            "thread/turns/list",
            json!({"threadId":id,"limit":1,"sortDirection":"desc"}),
        )
        .await
        .map_err(api_error)?;
    thread["thread"]["latestRunStatus"] = turns["data"][0]["status"].clone();
    Ok(Json(json!({"current":thread["thread"]})))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keeps_complete_messages_and_excludes_internal_items() {
        let body = "A".repeat(6000);
        assert_eq!(
            message(&json!({"item":{"id":"a","type":"agentMessage","text":body}})).unwrap()["text"],
            body
        );
        assert_eq!(
            message(&json!({"type":"userMessage","content":[{"type":"text","text":"My reply"}]}))
                .unwrap()["text"],
            "My reply"
        );
        assert!(message(&json!({"type":"reasoning","text":"private"})).is_none());
        assert!(message(&json!({"type":"hookPrompt"})).is_none());
    }
}
