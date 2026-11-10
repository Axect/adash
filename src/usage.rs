//! Per-session usage for the chat info panel: context fill, session totals, model, account
//! limits and cache state. Session files are read incrementally (offset + running totals).
use crate::util::{now_ms, LockExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

#[derive(Default, Clone)]
pub struct Acc {
    pub(crate) pos: crate::util::JsonlPos,
    pub(crate) turns: u64,
    pub(crate) input: i64,
    pub(crate) output: i64,
    pub(crate) cache_read: i64,
    pub(crate) cache_write: i64,
    pub(crate) cost: f64,
    pub(crate) first_ts: Option<i64>,
    pub(crate) last_ts: Option<i64>,
    pub(crate) compactions: u64,
    pub(crate) last_compaction: Option<i64>,
    /// last request: (input + cacheRead + cacheWrite, output)
    pub(crate) last_ctx: Option<(i64, i64)>,
    pub(crate) model: Option<String>,
    pub(crate) provider: Option<String>,
    pub(crate) credential: Option<i64>,
    /// codex: context window and rate limits as the harness reported them
    pub(crate) window: Option<i64>,
    pub(crate) rate_limits: Option<Value>,
    /// thinking / reasoning level as the session last recorded it
    pub(crate) thinking: Option<String>,
}

static FILES: LazyLock<Mutex<HashMap<PathBuf, Acc>>> = LazyLock::new(Default::default);

fn ts_of(v: &Value) -> Option<i64> {
    v["timestamp"].as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.timestamp_millis()).or_else(|| v["timestamp"].as_i64())
}

fn feed(kind: &str, v: &Value, a: &mut Acc) {
    if let Some(t) = ts_of(v) {
        a.first_ts.get_or_insert(t);
        a.last_ts = Some(t);
    }
    if let Some(h) = crate::harness::get(kind) {
        h.feed_usage(v, a);
    }
}

/// omp records: user turns, assistant usage/model/credential, compactions.
pub(crate) fn feed_omp(v: &Value, a: &mut Acc) {
    if v["type"] == "thinking_level_change" {
        a.thinking = v["thinkingLevel"].as_str().filter(|s| !s.is_empty()).map(String::from);
        return;
    }
    // a model switch counts from the moment it is recorded, not from the next answer
    if v["type"] == "model_change" {
        if let Some(m) = v["model"].as_str().filter(|m| !m.is_empty()) {
            match m.split_once('/') {
                Some((p, id)) => {
                    a.provider = Some(p.to_string());
                    a.model = Some(id.to_string());
                }
                None => a.model = Some(m.to_string()),
            }
            // the context window and prices follow the new model
            a.window = None;
        }
        return;
    }
    if v["type"] == "compaction" {
        a.compactions += 1;
        a.last_compaction = ts_of(v);
        return;
    }
    let m = &v["message"];
    match m["role"].as_str() {
        Some("user") => a.turns += 1,
        Some("assistant") => {
            let u = &m["usage"];
            if u.is_object() {
                let (i, o, cr, cw) = (
                    u["input"].as_i64().unwrap_or(0),
                    u["output"].as_i64().unwrap_or(0),
                    u["cacheRead"].as_i64().unwrap_or(0),
                    u["cacheWrite"].as_i64().unwrap_or(0),
                );
                a.input += i;
                a.output += o;
                a.cache_read += cr;
                a.cache_write += cw;
                a.cost += u["cost"]["total"].as_f64().unwrap_or(0.0);
                if i + cr + cw > 0 {
                    a.last_ctx = Some((i + cr + cw, o));
                }
            }
            if let Some(x) = m["model"].as_str() {
                a.model = Some(x.to_string());
            }
            if let Some(x) = m["provider"].as_str() {
                a.provider = Some(x.to_string());
            }
            if let Some(c) = m["credentialId"].as_i64() {
                a.credential = Some(c);
            }
        }
        _ => {}
    }
}

/// Claude Code records: user turns, assistant usage, compact boundaries.
pub(crate) fn feed_claude(v: &Value, a: &mut Acc) {
    match v["type"].as_str() {
        Some("user") if v["isMeta"] != true && !v["message"]["content"].as_array().is_some_and(|c| c.iter().any(|x| x["type"] == "tool_result")) => {
            a.turns += 1
        }
        Some("assistant") => {
            let u = &v["message"]["usage"];
            if u.is_object() {
                let (i, o, cr, cw) = (
                    u["input_tokens"].as_i64().unwrap_or(0),
                    u["output_tokens"].as_i64().unwrap_or(0),
                    u["cache_read_input_tokens"].as_i64().unwrap_or(0),
                    u["cache_creation_input_tokens"].as_i64().unwrap_or(0),
                );
                a.input += i;
                a.output += o;
                a.cache_read += cr;
                a.cache_write += cw;
                if i + cr + cw > 0 {
                    a.last_ctx = Some((i + cr + cw, o));
                }
            }
            if let Some(x) = v["message"]["model"].as_str().filter(|m| *m != "<synthetic>") {
                a.model = Some(x.to_string());
            }
            a.provider = Some("anthropic".into());
        }
        Some("system") if v["subtype"] == "compact_boundary" => {
            a.compactions += 1;
            a.last_compaction = ts_of(v);
        }
        _ => {}
    }
}

/// Codex rollout records: cumulative token totals, context window, rate limits.
pub(crate) fn feed_codex(v: &Value, a: &mut Acc) {
    let p = &v["payload"];
    match (v["type"].as_str(), p["type"].as_str()) {
        (Some("event_msg"), Some("user_message")) => a.turns += 1,
        (Some("event_msg"), Some("token_count")) => {
            let t = &p["info"]["total_token_usage"];
            if t.is_object() {
                // totals are cumulative: take them as they are
                let cached = t["cached_input_tokens"].as_i64().unwrap_or(0);
                a.input = t["input_tokens"].as_i64().unwrap_or(0) - cached;
                a.cache_read = cached;
                a.cache_write = t["cache_write_input_tokens"].as_i64().unwrap_or(0);
                a.output = t["output_tokens"].as_i64().unwrap_or(0);
            }
            let l = &p["info"]["last_token_usage"];
            if l.is_object() {
                a.last_ctx = Some((l["input_tokens"].as_i64().unwrap_or(0), l["output_tokens"].as_i64().unwrap_or(0)));
            }
            if let Some(w) = p["info"]["model_context_window"].as_i64() {
                a.window = Some(w);
            }
            if p["rate_limits"].is_object() {
                a.rate_limits = Some(p["rate_limits"].clone());
            }
        }
        (Some("turn_context"), _) => {
            if let Some(m) = p["model"].as_str() {
                a.model = Some(m.to_string());
            }
            if let Some(e) = p["effort"].as_str().filter(|s| !s.is_empty()) {
                a.thinking = Some(e.to_string());
            }
        }
        (Some("event_msg"), Some("thread_settings_applied")) => {
            if let Some(e) = p["thread_settings"]["reasoning_effort"].as_str().filter(|s| !s.is_empty()) {
                a.thinking = Some(e.to_string());
            }
        }
        (Some("compacted"), _) => {
            a.compactions += 1;
            a.last_compaction = ts_of(v);
        }
        _ => {}
    }
    a.provider = Some("openai".into());
}

fn read(kind: &str, path: &Path) -> Acc {
    let mut acc = FILES.plock().get(path).cloned().unwrap_or_default();
    if acc.pos.moved(path) {
        acc = Acc::default();
    }
    let before = acc.pos.offset;
    let mut pos = std::mem::take(&mut acc.pos);
    pos.read(path, |_, line| {
        if let Ok(v) = serde_json::from_slice::<Value>(line) {
            feed(kind, &v, &mut acc);
        }
    });
    acc.pos = pos;
    if acc.pos.offset != before {
        FILES.plock().insert(path.to_path_buf(), acc.clone());
    }
    acc
}

/// Context windows from `omp models --json` (selector and bare id), refreshed hourly.
fn windows() -> HashMap<String, i64> {
    static CACHE: LazyLock<Mutex<(i64, HashMap<String, i64>)>> = LazyLock::new(Default::default);
    let mut c = CACHE.plock();
    if now_ms() - c.0 > 3_600_000 || c.1.is_empty() {
        // the service's PATH may not hold omp: the same lookup the launcher uses
        let Some(bin) = crate::launch::find_bin("omp") else { return c.1.clone() };
        let out = crate::platform::timeout(20).arg(bin).args(["models", "--json"]).output();
        if let Ok(v) = out.map(|o| serde_json::from_slice::<Value>(&o.stdout).unwrap_or(Value::Null)) {
            let mut m = HashMap::new();
            let list = if v.is_array() { &v } else { &v["models"] };
            for x in list.as_array().into_iter().flatten() {
                if let Some(w) = x["contextWindow"].as_i64() {
                    for k in [&x["selector"], &x["id"]] {
                        if let Some(s) = k.as_str() {
                            m.entry(s.to_string()).or_insert(w);
                        }
                    }
                }
            }
            if !m.is_empty() {
                *c = (now_ms(), m);
            }
        }
    }
    c.1.clone()
}

/// "alice.dev@example.com" → "al***@example.com"
pub fn mask(email: &str) -> String {
    match email.split_once('@') {
        Some((u, d)) => format!("{}***@{d}", u.chars().take(2).collect::<String>()),
        None => format!("{}***", email.chars().take(3).collect::<String>()),
    }
}

/// Short window label: "Claude 5 Hour" / "5 Hour" → "5h", "7 days" → "7d".
fn short_window(label: &str) -> String {
    let l = label.to_ascii_lowercase();
    let n: String = l.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
    let unit = if l.contains("hour") || l.contains(" h") {
        "h"
    } else if l.contains("day") {
        "d"
    } else if l.contains("month") || l.contains("mo") {
        "mo"
    } else {
        ""
    };
    if n.is_empty() || unit.is_empty() {
        label.to_string()
    } else {
        format!("{n}{unit}")
    }
}

/// omp credential → account (cached 60 s: the panel polls).
fn omp_account(id: i64) -> Option<Value> {
    static CACHE: LazyLock<Mutex<HashMap<i64, (i64, Option<Value>)>>> = LazyLock::new(Default::default);
    if let Some((at, v)) = CACHE.plock().get(&id) {
        if now_ms() - at < 60_000 {
            return v.clone();
        }
    }
    let v = omp_account_read(id);
    CACHE.plock().insert(id, (now_ms(), v.clone()));
    v
}

/// omp credential → masked account and its latest limits (omp's own usage history).
fn omp_account_read(id: i64) -> Option<Value> {
    let db = rusqlite::Connection::open_with_flags(crate::util::home().join(".omp/agent/agent.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let (provider, data): (String, String) =
        db.query_row("SELECT provider, data FROM auth_credentials WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).ok()?;
    let d: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
    let email = d["email"].as_str().unwrap_or("").to_string();
    let account = d["accountId"].as_str().unwrap_or("").to_string();
    let mut st = db
        .prepare(
            "SELECT limit_id, label, window_label, used_fraction, resets_at, MAX(recorded_at) FROM usage_history
             WHERE provider = ?1 AND ((?2 != '' AND email = ?2) OR (?3 != '' AND account_id = ?3)) AND recorded_at > ?4
             GROUP BY limit_id ORDER BY limit_id",
        )
        .ok()?;
    let limits: Vec<Value> = st
        .query_map(rusqlite::params![provider, email, account, now_ms() - 7 * 86_400_000], |r| {
            let label: Option<String> = r.get(1)?;
            let window: Option<String> = r.get(2)?;
            // "Claude 7 Day (Fable)" keeps its qualifier: "7d Fable"
            let qual = label.as_deref().and_then(|l| l.split_once('(')).map(|x| format!(" {}", x.1.trim_end_matches(')'))).unwrap_or_default();
            Ok(json!({"id": r.get::<_, String>(0)?, "label": format!("{}{qual}", short_window(window.as_deref().or(label.as_deref()).unwrap_or(""))), "name": label, "pct": r.get::<_, Option<f64>>(3)?.map(|f| (f * 1000.0).round() / 10.0), "resetsAt": r.get::<_, Option<i64>>(4)?, "recordedAt": r.get::<_, i64>(5)?}))
        })
        .ok()?
        .filter_map(Result::ok)
        .collect();
    Some(json!({"provider": provider, "account": if email.is_empty() { mask(&account) } else { mask(&email) }, "credentialId": id, "windows": limits}))
}

fn codex_limits(r: &Value) -> Vec<Value> {
    ["primary", "secondary"]
        .iter()
        .filter_map(|k| {
            let x = &r[*k];
            x.is_object().then(|| {
                let mins = x["window_minutes"].as_i64().unwrap_or(0);
                json!({"id": format!("codex:{k}"), "label": if mins >= 1440 { format!("{}d", mins / 1440) } else { format!("{}h", mins / 60) }, "pct": x["used_percent"], "resetsAt": x["resets_at"].as_i64().map(|s| s * 1000)})
            })
        })
        .collect()
}

const LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// `gemini-4-argon-medium` → `medium` (only a known level name counts).
fn level_suffix(model: &str) -> Option<String> {
    model.rsplit(['-', ':']).next().filter(|l| LEVELS.contains(l)).map(String::from)
}

/// `--thinking <level>` / `--thinking=<level>` of the agent's process (process list cached 30 s).
fn launch_thinking(handle: &str) -> Option<String> {
    type Procs = (i64, HashMap<String, Vec<crate::session::Proc>>);
    static P: LazyLock<Mutex<Procs>> = LazyLock::new(Default::default);
    let mut c = P.plock();
    if now_ms() - c.0 > 30_000 {
        *c = (now_ms(), crate::session::scan_procs());
    }
    c.1.get(handle)?.iter().rev().find_map(|p| thinking_flag(&p.args))
}

fn thinking_flag(args: &[String]) -> Option<String> {
    args.iter().enumerate().find_map(|(i, x)| match x.strip_prefix("--thinking") {
        Some("") => args.get(i + 1).cloned(),
        Some(r) => r.strip_prefix('=').map(String::from),
        None => None,
    })
}

/// The usage view of one agent (`agents[].usage` and GET /api/agents/{name}/usage); parts
/// that do not apply are null.
pub fn view(a: &crate::collect::Agent) -> Value {
    let kind = a.agent_type.as_str();
    let Some(path) = a.session_path.as_deref().filter(|_| matches!(kind, "omp" | "claude" | "codex")) else {
        // Antigravity names the level in the model (`gemini-4-argon-medium`)
        let thinking = if kind == "antigravity" { a.model.as_deref().and_then(level_suffix) } else { None };
        return json!({"context": null, "session": null, "model": a.model, "thinking": thinking, "account": null});
    };
    let acc = read(kind, path);
    // the session's own record, else omp's `--thinking` launch flag; never guessed
    let thinking = acc.thinking.clone().or_else(|| if kind == "omp" { a.handle.as_deref().and_then(launch_thinking) } else { None });
    let model = acc.model.clone().or_else(|| a.model.clone());
    let limit = acc.window.or_else(|| {
        let w = windows();
        let sel = acc.provider.as_ref().zip(model.as_ref()).map(|(p, m)| format!("{p}/{m}"));
        sel.and_then(|s| w.get(&s).copied()).or_else(|| model.as_ref().and_then(|m| w.get(m).copied())).or_else(|| (kind == "claude").then_some(200_000))
    });
    // omp's status line: last request's prompt (input + cache read + cache write) over the
    // model's context window (checked: 551,005 tokens = "55.1%/1M")
    let context = acc.last_ctx.map(|(inp, out)| {
        let used = inp + if kind == "codex" { out } else { 0 };
        json!({"used": used, "limit": limit, "pct": limit.map(|w| ((used as f64 / w as f64) * 1000.0).round() / 10.0), "compactions": acc.compactions, "lastCompactionAt": acc.last_compaction})
    });
    let account = match kind {
        "omp" => acc.credential.and_then(omp_account),
        "codex" => acc.rate_limits.as_ref().map(|r| json!({"provider": "openai", "account": null, "windows": codex_limits(r)})),
        _ => None,
    };
    json!({
        "context": context,
        "session": {
            "turns": acc.turns, "input": acc.input, "output": acc.output, "cacheRead": acc.cache_read, "cacheWrite": acc.cache_write,
            "cost": (acc.cost * 1000.0).round() / 1000.0, "startedAt": acc.first_ts,
        },
        "model": model, "provider": acc.provider, "thinking": thinking,
        "account": account,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_context_and_compactions_from_records() {
        let mut a = Acc::default();
        for l in [
            r#"{"type":"message","timestamp":"2026-10-31T00:00:00Z","message":{"role":"user","content":[]}}"#,
            r#"{"type":"message","timestamp":"2026-10-31T00:01:00Z","message":{"role":"assistant","provider":"anthropic","model":"claude-opus-5-5","credentialId":12,"usage":{"input":4,"output":10,"cacheRead":1000,"cacheWrite":20,"cost":{"total":0.5}}}}"#,
            r#"{"type":"compaction","timestamp":"2026-10-31T00:02:00Z"}"#,
            r#"{"type":"message","timestamp":"2026-10-31T00:03:00Z","message":{"role":"assistant","usage":{"input":1,"output":5,"cacheRead":300,"cacheWrite":0,"cost":{"total":0.25}}}}"#,
        ] {
            feed("omp", &serde_json::from_str(l).unwrap(), &mut a);
        }
        assert_eq!((a.turns, a.input, a.output, a.cache_read, a.cache_write), (1, 5, 15, 1300, 20));
        assert_eq!((a.compactions, a.last_ctx, a.credential), (1, Some((301, 5)), Some(12)));
        assert!((a.cost - 0.75).abs() < 1e-9);
        let mut c = Acc::default();
        feed("codex", &serde_json::from_str(r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":60,"output_tokens":7},"last_token_usage":{"input_tokens":40,"output_tokens":3},"model_context_window":258400},"rate_limits":{"primary":{"used_percent":48.0,"window_minutes":10080,"resets_at":1793862443}}}}"#).unwrap(), &mut c);
        assert_eq!((c.input, c.cache_read, c.window), (40, 60, Some(258400)));
        assert_eq!(codex_limits(c.rate_limits.as_ref().unwrap())[0]["label"], "7d");
        assert_eq!((short_window("5 Hour"), short_window("Claude 7 Day")), ("5h".to_string(), "7d".to_string()));
        assert_eq!(mask("alice.dev@example.com"), "al***@example.com");
    }

    #[test]
    fn thinking_level_comes_from_the_session_or_the_launch_flag() {
        let mut o = Acc::default();
        for l in [r#"{"type":"thinking_level_change","thinkingLevel":"medium"}"#, r#"{"type":"thinking_level_change","thinkingLevel":"low"}"#] {
            feed("omp", &serde_json::from_str(l).unwrap(), &mut o);
        }
        assert_eq!(o.thinking.as_deref(), Some("low"), "latest change wins");
        let mut c = Acc::default();
        feed("codex", &serde_json::from_str(r#"{"type":"turn_context","payload":{"model":"gpt-6-sol","effort":"xhigh"}}"#).unwrap(), &mut c);
        assert_eq!(c.thinking.as_deref(), Some("xhigh"));
        feed(
            "codex",
            &serde_json::from_str(r#"{"type":"event_msg","payload":{"type":"thread_settings_applied","thread_settings":{"reasoning_effort":"medium"}}}"#)
                .unwrap(),
            &mut c,
        );
        assert_eq!(c.thinking.as_deref(), Some("medium"));
        let mut cl = Acc::default();
        feed("claude", &serde_json::from_str(r#"{"type":"assistant","message":{"model":"claude-opus-5-5","usage":{}}}"#).unwrap(), &mut cl);
        assert_eq!(cl.thinking, None, "Claude Code records no level: null, not a guess");
        assert_eq!(level_suffix("gemini-4-argon-medium").as_deref(), Some("medium"));
        assert_eq!(level_suffix("gemini-4-argon"), None);
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(thinking_flag(&args(&["omp", "--model", "x", "--thinking", "high"])).as_deref(), Some("high"));
        assert_eq!(thinking_flag(&args(&["omp", "--thinking=low"])).as_deref(), Some("low"));
        assert_eq!(thinking_flag(&args(&["omp", "--model", "x"])), None);
    }

    /// 2026-11-10: after `/model` the badge kept the old model until the next answer.
    #[test]
    fn a_model_switch_counts_before_the_next_answer() {
        let mut a = Acc::default();
        for l in [
            r#"{"type":"message","timestamp":"2026-11-10T02:40:00Z","message":{"role":"assistant","provider":"anthropic","model":"claude-opus-5-5","usage":{}}}"#,
            r#"{"type":"model_change","timestamp":"2026-11-10T02:45:28Z","model":"anthropic/claude-fable-5-2","role":"temporary"}"#,
        ] {
            feed("omp", &serde_json::from_str(l).unwrap(), &mut a);
        }
        assert_eq!((a.model.as_deref(), a.provider.as_deref()), (Some("claude-fable-5-2"), Some("anthropic")));
        feed(
            "omp",
            &serde_json::from_str(r#"{"type":"message","message":{"role":"assistant","provider":"openai","model":"gpt-6-sol","usage":{}}}"#).unwrap(),
            &mut a,
        );
        assert_eq!(a.model.as_deref(), Some("gpt-6-sol"), "a later answer wins again");
    }
}
