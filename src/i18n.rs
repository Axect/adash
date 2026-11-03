//! `?lang=en`: English for labels the server generates (Korean stays the default). Agent
//! message bodies, provider error texts and file contents are never translated.
use serde_json::{json, Value};

/// `agent.retry` in English.
pub fn retry_en(agent_type: &str) -> Value {
    if crate::server::retry_mode(agent_type) == "key" {
        json!({"label": "Retry (F5)", "how": "Sends F5 to omp (retry the last failed turn)"})
    } else {
        json!({"label": format!("Retry ({agent_type})"), "how": format!("{agent_type} has no retry key; sends the message '{}'", crate::server::RETRY_MESSAGE)})
    }
}

/// One server-written phrase in English (unknown text unchanged).
pub fn phrase(t: &str) -> String {
    if let Some(tool) = t.strip_suffix(" 승인 대기") {
        return format!("Awaiting approval: {tool}");
    }
    if let Some(name) = t.strip_prefix("스킬 ").and_then(|r| r.strip_suffix(" 불러옴")) {
        return format!("Loaded skill {name}");
    }
    if let Some(rest) = t.strip_prefix("오류: ") {
        return format!("Error: {rest}");
    }
    if let Some(rest) = t.strip_prefix("오류") {
        // "오류 403 · message" / "오류 · message"
        if rest.starts_with(' ') || rest.starts_with(" ·") {
            return format!("Error{rest}");
        }
    }
    match t {
        "24시간 동안 전달하지 못했습니다" => "Not delivered within 24 hours".into(),
        // cache TTL basis (meta::ttl_basis)
        "omp+Anthropic: 1시간 캐시(cacheRetention auto), 실측 약 60분" => "omp+Anthropic: 1 h cache (cacheRetention auto), measured ~60 min".into(),
        "Claude Code: 1시간 캐시(ephemeral_1h)" => "Claude Code: 1 h cache (ephemeral_1h)".into(),
        "OpenAI: 고정 TTL 없음, 실측 10–40분 적중 약 70%, 40분 넘으면 대부분 식음" => {
            "OpenAI: no fixed TTL; measured ~70% hits at 10–40 min, mostly cold after 40 min".into()
        }
        "z.ai: 실측 5분 뒤 절반 이하" => "z.ai: measured, under half after 5 min".into(),
        "Gemini: 문서상 TTL 없음, 보수적 5분" => "Gemini: no documented TTL; 5 min (conservative)".into(),
        "기본 5분" => "default 5 min".into(),
        "첫 메시지를 보내면 대화가 여기에 표시됩니다" => "The conversation appears here once you send the first message".into(),
        "미상" => "unknown".into(),
        _ => t.to_string(),
    }
}

/// Error strings returned by the API.
pub fn error_en(e: &str) -> String {
    if let Some(name) = e.strip_suffix(" 입력창에 보내지 않은 초안이 있어 재시도하지 않았습니다") {
        return format!("{name} has an unsent draft in its input box; not retried");
    }
    phrase(e)
}

fn ask_en(q: &mut Value) {
    if let Some(s) = q["question"].as_str() {
        q["question"] = json!(phrase(s));
    }
    for x in q["questions"].as_array_mut().into_iter().flatten() {
        if let Some(s) = x["question"].as_str() {
            x["question"] = json!(phrase(s));
        }
    }
}

fn messages_en(v: &mut Value) {
    for m in v["messages"].as_array_mut().into_iter().flatten() {
        // only what the server wrote: system lines, tool error summaries, provider-error cards
        if matches!(m["role"].as_str(), Some("system" | "tool" | "error")) {
            if let Some(t) = m["text"].as_str() {
                m["text"] = json!(phrase(t));
            }
        }
    }
}

pub fn english(segs: &[&str], v: &mut Value) {
    match segs {
        ["api", "state"] => {
            for a in v["agents"].as_array_mut().into_iter().flatten() {
                if a["retry"].is_object() {
                    let t = a["agentType"].as_str().unwrap_or("").to_string();
                    a["retry"] = retry_en(&t);
                }
                if a["ask"].is_object() {
                    ask_en(&mut a["ask"]);
                }
                if let Some(b) = a["cache"]["basis"].as_str() {
                    a["cache"]["basis"] = json!(phrase(b));
                }
            }
            for r in v["magiRuns"].as_array_mut().into_iter().flatten() {
                if let Some(e) = r["error"].as_str() {
                    r["error"] = json!(phrase(e));
                }
            }
        }
        ["api", "agents", _, "transcript"] | ["api", "sessions", _, "transcript"] => {
            messages_en(v);
            if let Some(h) = v["hint"].as_str() {
                v["hint"] = json!(phrase(h));
            }
        }
        ["api", "agents", _, "commands"] => {
            for p in v["prefixes"].as_array_mut().into_iter().flatten() {
                let h = v_harness(p);
                let w = match p["kind"].as_str() {
                    Some("shell") => format!("Starting with {} runs a shell command in {h}", p["prefix"].as_str().unwrap_or("")),
                    Some("python") => format!("Starting with '{}' runs Python in {h}", p["prefix"].as_str().unwrap_or("")),
                    Some("memory") => format!("Starting with {} saves to {h} memory", p["prefix"].as_str().unwrap_or("")),
                    _ => continue,
                };
                p["warning"] = json!(w);
            }
        }
        ["api", "agents", _, "retry"] => {
            if let Some(m) = v["error"]["message"].as_str() {
                v["error"]["message"] = json!(error_en(m));
            }
        }
        ["api", "retry-errors"] => {
            for r in v["results"].as_array_mut().into_iter().flatten() {
                if let Some(m) = r["error"]["message"].as_str() {
                    r["error"]["message"] = json!(error_en(m));
                }
            }
        }
        _ => {}
    }
}

/// The harness named in a Korean prefix warning ("! 로 시작하면 omp 가 …").
fn v_harness(p: &Value) -> String {
    p["warning"].as_str().and_then(|w| w.split(" 가 ").next()).and_then(|x| x.split_whitespace().last()).unwrap_or("the agent").to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn english_labels_leave_agent_text_alone() {
        let mut s = json!({"agents": [{"agentType": "omp", "retry": {"label": "F5 재시도"}, "ask": {"question": "bash 승인 대기", "questions": []}, "lastMessage": "한국어 답변"}], "magiRuns": [{"error": "24시간 동안 전달하지 못했습니다"}]});
        super::english(&["api", "state"], &mut s);
        assert_eq!(s["agents"][0]["retry"]["label"], "Retry (F5)");
        assert_eq!(s["agents"][0]["ask"]["question"], "Awaiting approval: bash");
        assert_eq!(s["agents"][0]["lastMessage"], "한국어 답변");
        assert_eq!(s["magiRuns"][0]["error"], "Not delivered within 24 hours");
        let mut c2 = json!({"agents": [{"cache": {"basis": "Claude Code: 1시간 캐시(ephemeral_1h)"}}]});
        super::english(&["api", "state"], &mut c2);
        assert_eq!(c2["agents"][0]["cache"]["basis"], "Claude Code: 1 h cache (ephemeral_1h)");
        let mut t = json!({"messages": [{"role": "error", "text": "오류 403 · Request not allowed"}, {"role": "system", "text": "스킬 magi 불러옴"}, {"role": "assistant", "text": "오류 없음"}]});
        super::english(&["api", "agents", "x", "transcript"], &mut t);
        assert_eq!(t["messages"][0]["text"], "Error 403 · Request not allowed");
        assert_eq!(t["messages"][1]["text"], "Loaded skill magi");
        assert_eq!(t["messages"][2]["text"], "오류 없음");
        let mut c = json!({"prefixes": [{"prefix": "!", "kind": "shell", "warning": "! 로 시작하면 omp 가 셸 명령으로 실행합니다"}]});
        super::english(&["api", "agents", "x", "commands"], &mut c);
        assert_eq!(c["prefixes"][0]["warning"], "Starting with ! runs a shell command in omp");
    }
}
