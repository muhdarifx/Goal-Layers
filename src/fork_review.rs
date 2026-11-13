//! Review in an owned read-only fork; persist results before deleting that fork.
use crate::*;

pub async fn stop(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    if headers.get("x-goal-atlas").and_then(|v| v.to_str().ok()) != Some("1") {
        return Err((StatusCode::FORBIDDEN, "Missing local client header".into()));
    }
    let key = format!("{id}:{}", text(&body, "request_id"));
    let receipt = app
        .message_receipts
        .lock()
        .await
        .get(&key)
        .cloned()
        .ok_or((StatusCode::NOT_FOUND, "Review not found".into()))?;
    if receipt["thread_id"] != id || receipt["transport"] != "fork" {
        return Err((StatusCode::BAD_REQUEST, "Not an owned review".into()));
    }
    if !matches!(
        text(&receipt, "status"),
        "queued" | "delivered" | "awaiting_status"
    ) {
        return Ok(Json(receipt));
    }
    let value = update(&app, &key, json!({"stop_requested":true,"message":"Stopping the review fork. The original chat is unchanged."})).await.map_err(api_error)?;
    Ok(Json(value))
}

async fn stop_requested(app: &App, key: &str) -> bool {
    app.message_receipts
        .lock()
        .await
        .get(key)
        .is_some_and(|r| r["stop_requested"] == true)
}

async fn mark_stopped(app: &App, key: &str) -> Result<(), Error> {
    update(app,key,json!({"status":"cancelled","message":"Review stopped. The fork is retained for inspection; the original chat is unchanged."})).await?;
    Ok(())
}

pub async fn snapshots(app: &App, thread: &str) -> Vec<Value> {
    app.message_receipts
        .lock()
        .await
        .values()
        .filter(|r| r["thread_id"] == thread && r["transport"] == "fork")
        .map(|r| {
            let mut public = serde_json::Map::new();
            for key in [
                "thread_id",
                "request_id",
                "action",
                "review_kind",
                "transport",
                "status",
                "message",
                "fork_id",
                "fork_deleted",
                "requested_at",
                "added_work_summary",
                "progress",
            ] {
                if let Some(value) = r.get(key) {
                    public.insert(key.into(), value.clone());
                }
            }
            Value::Object(public)
        })
        .collect()
}

pub async fn saved_rows(app: &App, thread: &str) -> Vec<Value> {
    app.message_receipts
        .lock()
        .await
        .values()
        .filter(|r| r["thread_id"] == thread && r["transport"] == "fork")
        .flat_map(|r| r["response_rows"].as_array().cloned().unwrap_or_default())
        .collect()
}

pub async fn resume_pending(app: &Arc<App>, thread: &str) {
    let keys: Vec<_> = app
        .message_receipts
        .lock()
        .await
        .iter()
        .filter(|(_, r)| {
            r["thread_id"] == thread
                && r["transport"] == "fork"
                && r["fork_deleted"] != true
                && matches!(
                    text(r, "status"),
                    "queued" | "delivered" | "awaiting_status" | "applied"
                )
        })
        .map(|(k, _)| k.clone())
        .collect();
    for key in keys {
        if !app.flow.running_reviews.lock().await.insert(key.clone()) {
            continue;
        }
        let app = app.clone();
        tokio::spawn(async move {
            if let Err(error) = run(&app, &key).await {
                let mut receipts = app.message_receipts.lock().await;
                if let Some(r) = receipts.get_mut(&key) {
                    if r["status"] == "applied" {
                        r["cleanup_error"] = json!(error.to_string());
                    } else {
                        r["status"] = json!("rejected");
                        r["message"] = json!(format!(
                            "Review fork retained: {error}. The original chat was not changed."
                        ));
                    }
                    let _ = chat_actions::save_receipts(&app, &receipts).await;
                }
            }
            app.flow.running_reviews.lock().await.remove(&key);
            let _ = app.updates.send(now());
        });
    }
}

async fn update(app: &App, key: &str, fields: Value) -> Result<Value, Error> {
    let mut receipts = app.message_receipts.lock().await;
    let r = receipts.get_mut(key).ok_or("Review receipt disappeared")?;
    for (k, v) in fields.as_object().ok_or("Invalid receipt update")? {
        r[k] = v.clone();
    }
    let result = r.clone();
    chat_actions::save_receipts(app, &receipts)
        .await
        .map_err(|(_, e)| e)?;
    drop(receipts);
    if let Some(message) = fields["message"].as_str() {
        if let Some(job) = app
            .flow
            .jobs
            .lock()
            .await
            .get_mut(text(&result, "thread_id"))
        {
            job.message = message.to_owned();
        }
    }
    let _ = app.updates.send(now());
    Ok(result)
}

fn owned_fork(receipt: &Value) -> Result<&str, Error> {
    let fork = receipt["fork_id"].as_str().ok_or("Missing review fork")?;
    if fork.is_empty()
        || fork == text(receipt, "thread_id")
        || receipt["fork_parent"] != receipt["thread_id"]
    {
        return Err("Refusing to operate on an unowned fork or original chat".into());
    }
    Ok(fork)
}

fn read_only(response: &Value) -> Result<(), Error> {
    if response["sandbox"]["type"] != "readOnly" || response["sandbox"]["networkAccess"] == true {
        return Err("Codex did not confirm a read-only, network-restricted fork".into());
    }
    Ok(())
}

fn fastest_tier(model: &Value, requirements: &Value) -> (&'static str, &'static str) {
    let features = &requirements["requirements"]["featureRequirements"];
    let available = |id: &str| {
        model["serviceTiers"]
            .as_array()
            .is_some_and(|tiers| tiers.iter().any(|t| t["id"] == id))
    };
    if features["fast_mode"] != false
        && features["ultrafast_mode"] != false
        && available("ultrafast")
    {
        return ("ultrafast", "Ultrafast");
    }
    if features["fast_mode"] != false && (available("priority") || available("fast")) {
        return (
            if available("priority") {
                "priority"
            } else {
                "fast"
            },
            "Fast",
        );
    }
    ("default", "Standard")
}

pub async fn review_options(app: &App, thread: &str) -> Result<Value, Error> {
    let mut client = app.codex.lock().await;
    let account = client.call("account/read", json!({})).await?;
    let config = client
        .call("config/read", json!({"includeLayers":false}))
        .await?;
    let requirements = client.call("configRequirements/read", Value::Null).await?;
    let original = client
        .call(
            "thread/read",
            json!({"threadId":thread,"includeTurns":false}),
        )
        .await?;
    let wanted = original["thread"]["model"]
        .as_str()
        .or_else(|| config["config"]["model"].as_str());
    let mut cursor = Value::Null;
    loop {
        let models = client
            .call(
                "model/list",
                json!({"includeHidden":true,"limit":100,"cursor":cursor}),
            )
            .await?;
        if let Some(model) = models["data"].as_array().and_then(|models| {
            models.iter().find(|m| {
                wanted.map_or(m["isDefault"] == true, |id| {
                    m["model"] == id || m["id"] == id
                })
            })
        }) {
            let (tier, speed) = fastest_tier(model, &requirements);
            return Ok(
                json!({"model":model["model"],"service_tier":tier,"speed":speed,"account_type":account["account"]["type"],"plan":account["account"]["planType"]}),
            );
        }
        cursor = models["nextCursor"].clone();
        if cursor.is_null() {
            return Err("Configured model is not available in the account's model catalog".into());
        }
    }
}

fn progress_message(turn: &Value) -> Option<String> {
    turn["items"].as_array()?.iter().rev().find_map(|item| {
        let message = item["text"].as_str()?.trim();
        (item["type"] == "agentMessage" && item["phase"] == "commentary" && !message.is_empty())
            .then(|| message.chars().take(600).collect())
    })
}

async fn run(app: &Arc<App>, key: &str) -> Result<(), Error> {
    let mut receipt = app
        .message_receipts
        .lock()
        .await
        .get(key)
        .cloned()
        .ok_or("Missing review receipt")?;
    let original = text(&receipt, "thread_id").to_owned();
    let mut client = Codex::connect().await?;
    run_with_client(app, key, &mut client, &mut receipt, &original).await?;
    client._child.kill().await?;
    client._child.wait().await?;
    receipt = app
        .message_receipts
        .lock()
        .await
        .get(key)
        .cloned()
        .ok_or("Missing review receipt")?;
    if receipt["status"] == "applied" {
        // Release the fork's writer before deleting it from an independent connection.
        let mut cleaner = Codex::connect().await?;
        cleanup(app, key, &mut cleaner, &receipt).await?;
    }
    Ok(())
}

async fn run_with_client(
    app: &Arc<App>,
    key: &str,
    client: &mut Codex,
    receipt: &mut Value,
    original: &str,
) -> Result<(), Error> {
    if stop_requested(app, key).await {
        return mark_stopped(app, key).await;
    }
    if receipt["status"] == "applied" && receipt["response_saved_at"].is_number() {
        return Ok(());
    }
    if receipt["fork_id"].is_null() {
        let cwd = scope_breakdown::project_directory(&*app.catalog.read().await, original)
            .ok_or("Original project directory unavailable")?;
        let config = client
            .call("config/read", json!({"includeLayers":false}))
            .await?;
        let mut overrides = serde_json::Map::new();
        overrides.insert("features.apps".into(), json!(false));
        overrides.insert("features.multi_agent".into(), json!(false));
        if let Some(servers) = config["config"]["mcp_servers"].as_object() {
            for name in servers.keys() {
                if !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                {
                    return Err(
                        "Unsupported MCP server name; cannot safely disable its tools".into(),
                    );
                }
                overrides.insert(format!("mcp_servers.{name}.enabled"), json!(false));
            }
        }
        let fork=client.call("thread/fork",json!({"threadId":original,"cwd":cwd,"sandbox":"read-only","approvalPolicy":"never","deferGoalContinuation":true,"config":overrides,"excludeTurns":true})).await?;
        let id = fork["thread"]["id"]
            .as_str()
            .ok_or("Codex returned no fork ID")?;
        if id == original {
            return Err("Codex returned the original ID instead of a fork".into());
        }
        *receipt = update(app, key, json!({"fork_id":id,"fork_parent":original})).await?;
        read_only(&fork)?;
    } else {
        let fork = owned_fork(&receipt)?;
        let resumed = client
            .call(
                "thread/resume",
                json!({"threadId":fork,"sandbox":"read-only","approvalPolicy":"never"}),
            )
            .await?;
        read_only(&resumed)?;
    }
    let fork = owned_fork(&receipt)?.to_owned();
    // An inherited active goal must never continue after the analysis turn.
    client
        .call("thread/goal/clear", json!({"threadId":fork}))
        .await?;
    if receipt["response_rows"].as_array().is_none() {
        let request = text(&receipt, "request_id");
        let state = client
            .call("thread/read", json!({"threadId":fork,"includeTurns":true}))
            .await?;
        let previous = state["thread"]["turns"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|turn| {
                turn["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|i| scope_breakdown::is_request(i, request))
            });
        let turn_id = if let Some(turn) = previous {
            text(turn, "id").to_owned()
        } else {
            let started=client.call("turn/start",json!({"threadId":fork,"clientUserMessageId":request,"approvalPolicy":"never","model":receipt["review_options"]["model"],"serviceTierForTurn":receipt["review_options"]["service_tier"],"sandboxPolicy":{"type":"readOnly","networkAccess":false},"input":[{"type":"text","text":format!("You are the temporary read-only review fork of {original}. Never execute the original task, modify files, contact external services, create agents, or update a goal. Return the requested envelopes using the ORIGINAL thread_id {original}, not this fork ID.\n\n{}",text(&receipt,"prompt"))}]})).await?;
            started["turn"]["id"]
                .as_str()
                .ok_or("Review turn ID missing")?
                .to_owned()
        };
        update(app,key,json!({"status":"delivered","fork_turn_id":turn_id,"message":"Reviewing in a read-only fork. The original chat is unchanged."})).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1800);
        let mut last_progress = Value::Null;
        loop {
            if stop_requested(app, key).await {
                client
                    .call("turn/interrupt", json!({"threadId":fork,"turnId":turn_id}))
                    .await?;
                return mark_stopped(app, key).await;
            }
            let state = client
                .call("thread/read", json!({"threadId":fork,"includeTurns":true}))
                .await?;
            let turn = state["thread"]["turns"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|t| t["id"] == turn_id);
            if let Some(turn) = turn {
                let items = turn["items"].as_array();
                let operations = |status: &str| {
                    items
                        .into_iter()
                        .flatten()
                        .filter(|i| {
                            matches!(
                                text(i, "type"),
                                "commandExecution" | "mcpToolCall" | "webSearch"
                            ) && text(i, "status") == status
                        })
                        .count()
                };
                let progress = json!({"status":turn["status"],"completed":operations("completed"),"running":operations("inProgress")});
                let mut fields = json!({"progress":progress});
                if let Some(message) = progress_message(turn) {
                    fields["message"] = json!(message);
                }
                if fields != last_progress {
                    update(app, key, fields.clone()).await?;
                    last_progress = fields;
                }
                match text(turn, "status") {
                    "completed" => {
                        let rows: Vec<_> = turn["items"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|i| i["type"] == "agentMessage")
                            .cloned()
                            .map(|i| json!({"item":i}))
                            .collect();
                        if rows.is_empty() {
                            return Err("Review completed without a response".into());
                        }
                        *receipt = update(
                            app,
                            key,
                            json!({"response_rows":rows,"response_saved_at":now()}),
                        )
                        .await?;
                        break;
                    }
                    "failed" | "interrupted" => {
                        return Err("Review turn failed; saved fork kept for inspection".into());
                    }
                    _ => {}
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("Review timed out; fork kept for recovery".into());
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
    let rows = receipt["response_rows"]
        .as_array()
        .ok_or("No saved review response")?;
    status_updates::apply(app, &original, rows)
        .await
        .map_err(|(_, e)| e)?;
    scope_breakdown::apply(app, &original, rows)
        .await
        .map_err(|(_, e)| e)?;
    *receipt = app
        .message_receipts
        .lock()
        .await
        .get(key)
        .cloned()
        .ok_or("Missing review receipt")?;
    if receipt["status"] != "applied" {
        return Ok(());
    }
    Ok(())
}

async fn cleanup(app: &App, key: &str, client: &mut Codex, receipt: &Value) -> Result<(), Error> {
    let fork = owned_fork(receipt)?;
    if receipt["status"] != "applied" || receipt["response_saved_at"].is_null() {
        return Err("Refusing cleanup before response is saved".into());
    }
    if let Err(error) = client.call("thread/delete", json!({"threadId":fork})).await {
        if !error.to_string().contains("no rollout found for thread id") {
            return Err(error);
        }
    }
    update(app,key,json!({"fork_deleted":true,"cleanup_error":null,"message":"Deep review saved and validated. Temporary fork deleted; original chat unchanged."})).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fastest_available_speed_respects_catalog_and_restrictions() {
        let model = json!({"serviceTiers":[{"id":"priority"},{"id":"ultrafast"}]});
        assert_eq!(fastest_tier(&model, &json!({})).0, "ultrafast");
        assert_eq!(
            fastest_tier(
                &model,
                &json!({"requirements":{"featureRequirements":{"ultrafast_mode":false}}})
            )
            .0,
            "priority"
        );
        assert_eq!(
            fastest_tier(
                &model,
                &json!({"requirements":{"featureRequirements":{"fast_mode":false}}})
            )
            .0,
            "default"
        );
        assert_eq!(
            fastest_tier(&json!({"serviceTiers":[]}), &json!({})).0,
            "default"
        );
    }

    #[test]
    fn progress_is_latest_visible_commentary_not_final_payload() {
        let turn = json!({"items":[{"type":"agentMessage","phase":"commentary","text":"Reading documents"},{"type":"reasoning","text":"private"},{"type":"agentMessage","phase":"commentary","text":"Checked saved evidence"},{"type":"agentMessage","phase":"final_answer","text":"envelope"}]});
        assert_eq!(
            progress_message(&turn).as_deref(),
            Some("Checked saved evidence")
        );
        assert!(progress_message(&json!({"items":[]})).is_none());
        assert_eq!(progress_message(&json!({"items":[{"type":"agentMessage","phase":"commentary","text":"a".repeat(800)}]})).unwrap().len(),600);
    }
    #[test]
    fn cleanup_requires_owned_fork_and_read_only_policy() {
        assert!(
            owned_fork(
                &json!({"thread_id":"original","fork_id":"original","fork_parent":"original"})
            )
            .is_err()
        );
        assert!(
            owned_fork(&json!({"thread_id":"original","fork_id":"child","fork_parent":"other"}))
                .is_err()
        );
        assert!(read_only(&json!({"sandbox":{"type":"workspaceWrite"}})).is_err());
        assert!(read_only(&json!({"sandbox":{"type":"readOnly","networkAccess":true}})).is_err());
        assert!(read_only(&json!({"sandbox":{"type":"readOnly","networkAccess":false}})).is_ok());
    }

    #[tokio::test]
    async fn saves_validated_result_before_deleting_only_fork() {
        for mode in 0..3 {
            let status_only = mode == 1;
            let stopping = mode == 2;
            let dir = std::env::temp_dir().join(format!("fork-review-{}", now()));
            std::fs::create_dir_all(&dir).unwrap();
            let graph:flow::Graph=serde_json::from_value(json!({"summary":"Goal","nodes":[{"id":"root","kind":"goal","lane":"aligned","title":"Build a tool","status":"unknown","description":"","next":"","evidence":[{"source_id":"goal:original","quote":"Build a tool"}]}],"edges":[],"inspected_files":[],"limitations":[]})).unwrap();
            let report = json!({"version":1,"thread_id":"original","request_id":"request-id-123456","added_work_summary":{"text":"No added work.","evidence":[]},"graph":{"summary":"No missing nodes","nodes":[],"edges":[],"inspected_files":[],"limitations":[]}});
            let response = if status_only {
                format!(
                    "```goal-atlas-status\n{}\n```",
                    json!({"version":1,"thread_id":"original","request_id":"request-id-123456","updates":[{"node_id":"root","status":"complete","blockers":[],"next_step":"","evidence":"Recorded check passed"}]})
                )
            } else {
                format!("```goal-layers-scope\n{report}\n```")
            };
            let script = format!(
                r#"
const fs=require('fs'),readline=require('readline');let started=false,reads=0;
const dir={dir},response={response};
readline.createInterface({{input:process.stdin}}).on('line',line=>{{const r=JSON.parse(line);fs.appendFileSync(dir+'/calls.jsonl',line+'\n');let result={{}};
if(r.method==='config/read')result={{config:{{model:'review-model',mcp_servers:{{example:{{}}}}}}}};
else if(r.method==='account/read')result={{account:{{type:'chatgpt',planType:'test-plan'}}}};
else if(r.method==='model/list')result={{data:[{{model:'review-model',isDefault:true,serviceTiers:[{{id:'priority'}},{{id:'ultrafast'}}]}}],nextCursor:null}};
else if(r.method==='configRequirements/read')result={{requirements:null}};
else if(r.method==='thread/fork'||r.method==='thread/resume')result={{thread:{{id:'child'}},sandbox:{{type:'readOnly',networkAccess:false}}}};
else if(r.method==='turn/start'){{started=true;result={{turn:{{id:'review-turn'}}}};}}
else if(r.method==='thread/read'){{if(started&&++reads===1)result={{thread:{{turns:[{{id:'review-turn',status:'inProgress',items:[{{type:'agentMessage',phase:'commentary',text:'Checking saved evidence'}}]}}]}}}};else{{if(started&&JSON.parse(fs.readFileSync(dir+'/message-receipts.json'))['original:request-id-123456'].message!=='Checking saved evidence')throw Error('Progress was not saved');result={{thread:{{turns:started?[{{id:'review-turn',status:'completed',items:[{{type:'agentMessage',text:response}}]}}]:[]}}}};}}}}
else if(r.method==='thread/delete'){{const receipts=JSON.parse(fs.readFileSync(dir+'/message-receipts.json'));if(receipts['original:request-id-123456'].status!=='applied'||!receipts['original:request-id-123456'].response_saved_at)throw Error('Deleted before validation/save');}}
console.log(JSON.stringify({{id:r.id,result}}));}});
"#,
                dir = serde_json::to_string(dir.to_str().unwrap()).unwrap(),
                response = serde_json::to_string(&response).unwrap()
            );
            let mut child = Command::new("node")
                .args(["-e", &script])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let mut client = Codex {
                input: child.stdin.take().unwrap(),
                output: BufReader::new(child.stdout.take().unwrap()).lines(),
                _child: child,
                sequence: 0,
            };
            let mut catalog = Catalog::default();
            catalog
                .goals
                .insert("original".into(), json!({"objective":"Build a tool"}));
            catalog
                .threads
                .insert("original".into(), json!({"cwd":dir}));
            let (updates, _) = broadcast::channel(16);
            let mut receipt = json!({"thread_id":"original","request_id":"request-id-123456","action":"breakdown","transport":"fork","status":"queued","review_version":2,"baseline_graph":graph,"goal":"Build a tool","prompt":"Analyze only","requested_at":1});
            if status_only {
                receipt["action"] = json!("status");
                receipt["review_kind"] = json!("refresh_status");
                receipt["targets"] = json!(
                    flow::Document {
                        thread_id: "original".into(),
                        generated_at: 0,
                        source_count: 1,
                        source_threads: 1,
                        graph: graph.clone(),
                        mode: "local".into()
                    }
                    .nodes()
                );
            }
            // Use the same isolated protocol fixture for account capability inspection.
            let mut observer = Command::new("node")
                .args(["-e", &script])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let app = Arc::new(App {
                trace: Arc::new(std::sync::Mutex::new(trace::Cache::default())),
                flow: flow::Hub::load(dir.join("flows.json")).await.unwrap(),
                codex: Mutex::new(Codex {
                    input: observer.stdin.take().unwrap(),
                    output: BufReader::new(observer.stdout.take().unwrap()).lines(),
                    _child: observer,
                    sequence: 0,
                }),
                catalog: RwLock::new(catalog),
                nodes: Mutex::new(vec![]),
                message_receipts: Mutex::new(BTreeMap::from([(
                    "original:request-id-123456".into(),
                    receipt.clone(),
                )])),
                reports: RwLock::new(BTreeMap::new()),
                codex_home: dir.clone(),
                file: dir.join("nodes.json"),
                updates,
            });
            app.flow.documents.lock().await.insert(
                "original".into(),
                flow::Document {
                    thread_id: "original".into(),
                    generated_at: 0,
                    source_count: 1,
                    source_threads: 1,
                    graph,
                    mode: "local".into(),
                },
            );
            // Preflight may inspect capabilities, but must not queue or fork without consent.
            app.message_receipts.lock().await.clear();
            let mut headers = HeaderMap::new();
            headers.insert("x-goal-atlas", "1".parse().unwrap());
            let preflight=chat_actions::send(State(app.clone()),Path("original".into()),headers,Json(serde_json::from_value(json!({"action":"breakdown","request_id":"consent-test-123456","node_ids":[]})).unwrap())).await.unwrap().0;
            assert_eq!(preflight["status"], "confirmation_required");
            assert_eq!(preflight["options"]["service_tier"], "ultrafast");
            assert!(app.message_receipts.lock().await.is_empty());
            receipt["review_options"] = preflight["options"].clone();
            app.message_receipts
                .lock()
                .await
                .insert("original:request-id-123456".into(), receipt.clone());
            let pending = snapshots(&app, "original").await;
            assert_eq!(pending[0]["status"], "queued");
            assert!(pending[0].get("prompt").is_none());
            assert!(pending[0].get("baseline_graph").is_none());
            let mut events = app.updates.subscribe();
            let cancel_task = if stopping {
                let app = app.clone();
                Some(tokio::spawn(async move {
                    loop {
                        if app.message_receipts.lock().await["original:request-id-123456"]["message"]
                            == "Checking saved evidence"
                        {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    let mut headers = HeaderMap::new();
                    headers.insert("x-goal-atlas", "1".parse().unwrap());
                    let stopped = stop(
                        State(app),
                        Path("original".into()),
                        headers,
                        Json(json!({"request_id":"request-id-123456"})),
                    )
                    .await
                    .unwrap();
                    assert_eq!(stopped.0["stop_requested"], true);
                }))
            } else {
                None
            };
            let mut r = receipt;
            run_with_client(
                &app,
                "original:request-id-123456",
                &mut client,
                &mut r,
                "original",
            )
            .await
            .unwrap();
            if let Some(task) = cancel_task {
                task.await.unwrap();
                let saved = app.message_receipts.lock().await["original:request-id-123456"].clone();
                assert_eq!(saved["status"], "cancelled");
                assert_ne!(saved["fork_deleted"], true);
                let calls = std::fs::read_to_string(dir.join("calls.jsonl")).unwrap();
                assert!(calls.lines().any(|line| {
                    let c: Value = serde_json::from_str(line).unwrap();
                    c["method"] == "turn/interrupt" && c["params"]["threadId"] == "child"
                }));
                assert!(!calls.contains("thread/delete"));
                drop(client);
                drop(app);
                std::fs::remove_dir_all(&dir).unwrap();
                continue;
            }
            assert!(
                events.try_recv().is_ok(),
                "Receipt changes must notify the page"
            );
            let public = snapshots(&app, "original").await;
            assert_eq!(public[0]["fork_id"], "child");
            assert!(public[0].get("response_rows").is_none());
            let stored = app.message_receipts.lock().await["original:request-id-123456"].clone();
            cleanup(&app, "original:request-id-123456", &mut client, &stored)
                .await
                .unwrap();
            let saved = app.message_receipts.lock().await["original:request-id-123456"].clone();
            assert_eq!(saved["fork_deleted"], true);
            assert!(saved["response_rows"].as_array().unwrap().len() > 0);
            assert_eq!(
                saved_rows(&app, "original").await.len(),
                1,
                "Results must survive deletion for refresh and restart"
            );
            let calls: Vec<Value> = std::fs::read_to_string(dir.join("calls.jsonl"))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(
                calls
                    .iter()
                    .filter(|r| r["method"] == "thread/fork")
                    .count(),
                1
            );
            assert_eq!(
                calls.iter().filter(|r| r["method"] == "turn/start").count(),
                1
            );
            for call in calls.iter().filter(|r| {
                r["method"] != "config/read"
                    && r["method"] != "thread/fork"
                    && !matches!(
                        r["method"].as_str(),
                        Some("account/read" | "model/list" | "configRequirements/read")
                    )
            }) {
                if call["method"] == "thread/read" && call["params"]["includeTurns"] == false {
                    continue;
                }
                assert_eq!(call["params"]["threadId"], "child");
            }
            let start = calls.iter().find(|r| r["method"] == "turn/start").unwrap();
            assert_eq!(start["params"]["serviceTierForTurn"], "ultrafast");
            assert_eq!(start["params"]["model"], "review-model");
            let fork = calls.iter().find(|r| r["method"] == "thread/fork").unwrap();
            assert_eq!(fork["params"]["sandbox"], "read-only");
            assert_eq!(fork["params"]["deferGoalContinuation"], true);
            assert!(fork["params"]["config"].to_string().contains("mcp_servers"));
            client._child.kill().await.unwrap();
            client._child.wait().await.unwrap();
            drop(app);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
