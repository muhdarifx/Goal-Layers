use crate::{Codex, Json, Value, json};

// Only expose display fields, never authentication tokens or account identifiers.
fn display(account: &Value, limits: &Value) -> Value {
    let a = &account["account"];
    let bucket = limits["rateLimitsByLimitId"]
        .get("codex")
        .unwrap_or(&limits["rateLimits"]);
    let weekly = ["primary", "secondary"]
        .iter()
        .map(|key| &bucket[*key])
        .find(|window| window["windowDurationMins"].as_u64() == Some(10080));
    // Format the reported plan identifier without inferring a price.
    let plan = match a["planType"].as_str() {
        Some("promax") => json!("Pro - Max"),
        _ => a["planType"].clone(),
    };
    json!({"signedIn": !a.is_null(), "email": a["email"], "type": a["type"],
        "plan": plan, "credits": {
            "balance": bucket["credits"]["balance"],
            "unlimited": bucket["credits"]["unlimited"],
            "hasCredits": bucket["credits"]["hasCredits"]},
        "weekly": weekly.map(|w| json!({"usedPercent":w["usedPercent"],"resetsAt":w["resetsAt"]}))})
}

pub async fn read() -> Json<Value> {
    let result = async {
        // A dedicated read-only connection does not delay goal activity requests.
        let mut client = Codex::connect().await?;
        let account = client
            .call("account/read", json!({"refreshToken":false}))
            .await?;
        let limits = client.call("account/rateLimits/read", json!({})).await;
        let mut value = display(&account, limits.as_ref().unwrap_or(&Value::Null));
        if limits.is_err() {
            value["usageUnavailable"] = json!(true);
        }
        Ok::<Value, crate::Error>(value)
    };
    Json(
        match tokio::time::timeout(std::time::Duration::from_secs(15), result).await {
            Ok(Ok(value)) => value,
            _ => json!({"error":"Codex account is unavailable. Try again."}),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_weekly_window_and_safe_fields() {
        let a = json!({"account":{"email":"test@example.com","type":"chatgpt","planType":"pro","accessToken":"secret"}});
        let l = json!({"rateLimitsByLimitId":{"codex":{"primary":{"windowDurationMins":300,"usedPercent":10},"secondary":{"windowDurationMins":10080,"usedPercent":25,"resetsAt":2000000000},"credits":{"balance":"12.5","unlimited":false}}}});
        let d = display(&a, &l);
        assert_eq!(d["weekly"]["usedPercent"], 25);
        assert_eq!(d["credits"]["balance"], "12.5");
        assert!(!d.to_string().contains("secret"));
        assert_eq!(
            display(&json!({"account":{"planType":"promax"}}), &Value::Null)["plan"],
            "Pro - Max"
        );
        let d = display(
            &Value::Null,
            &json!({"rateLimits":{"secondary":{"usedPercent":20}}}),
        );
        assert_eq!(d["signedIn"], false);
        assert!(d["weekly"].is_null());
        assert!(d["credits"]["balance"].is_null());
    }
}

fn usage_html(value: &Value) -> String {
    let balance = if value["credits"]["unlimited"] == true {
        "Unlimited".to_owned()
    } else {
        value["credits"]["balance"]
            .as_f64()
            .or_else(|| {
                value["credits"]["balance"]
                    .as_str()
                    .and_then(|s| s.parse::<f64>().ok())
            })
            .filter(|v| v.is_finite())
            .map(|v| format!("{v:.2} credits"))
            .unwrap_or_else(|| "Unavailable".to_owned())
    };
    let percent = value["weekly"]["usedPercent"]
        .as_f64()
        .filter(|v| v.is_finite())
        .map(|v| (100.0 - v).clamp(0.0, 100.0));
    let remaining = percent
        .map(|v| format!("{v:.2}% remaining"))
        .unwrap_or_else(|| "Unavailable".to_owned());
    let plan = value["plan"]
        .as_str()
        .filter(|p| !p.is_empty())
        .unwrap_or("Unavailable")
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    let reset = value["weekly"]["resetsAt"].as_u64();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let reset_label = reset
        .map(|v| format!("In {} days", v.saturating_sub(now).div_ceil(86400)))
        .unwrap_or_else(|| "Unavailable".to_owned());
    let meter = percent.map(|v| { let bars: String = (0..20).map(|i| if (i as f64) < (v/5.0).round() { "<i class=\"lit\"></i>" } else { "<i></i>" }).collect(); format!("<div class=\"usage-meter\" role=\"meter\" aria-label=\"Weekly usage remaining\" aria-valuemin=\"0\" aria-valuemax=\"100\" aria-valuenow=\"{v}\">{bars}</div>") }).unwrap_or_default();
    let reset_attr = reset
        .map(|v| format!(" data-reset=\"{v}\""))
        .unwrap_or_default();
    format!(
        "<div class=\"account-row usage-balance\"><span>Credits</span><strong>{balance}</strong></div><div class=\"account-row usage-week\"><span>This week</span><strong>{remaining}</strong></div>{meter}<div class=\"account-row usage-plan\"><span>Bill with the plan</span><strong>{plan}</strong></div><div class=\"account-row usage-reset\"><span>Resets</span><strong{reset_attr}>{reset_label}</strong></div>"
    )
}

pub async fn page() -> axum::response::Html<String> {
    let Json(value) = read().await;
    axum::response::Html(
        include_str!("../web/index.html").replace("Loading usage…", &usage_html(&value)),
    )
}

#[cfg(test)]
mod page_tests {
    use super::*;
    #[test]
    fn initial_usage_has_real_values_and_safe_fallbacks() {
        let html = usage_html(
            &json!({"credits":{"balance":"1200.123456"},"weekly":{"usedPercent":72.1234,"resetsAt":2000000000}}),
        );
        assert!(html.contains("1200.12 credits"));
        assert!(html.contains("27.88% remaining"));
        assert_eq!(html.matches("class=\"lit\"").count(), 6);
        assert!(usage_html(&json!({"error":"offline"})).contains("Unavailable"));
        assert!(!usage_html(&json!({"credits":{"balance":"<script>"}})).contains("<script>"));
    }
}
