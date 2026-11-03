//! Hourly usage of one session (`GET /api/agents/{name}/usage/hourly?hours=48`): tokens, cost,
//! model replies, user messages and tool calls per local clock hour, read from the session file
//! with the same per-harness usage parsers as the chat info panel. Subagent files of the session
//! add to the hour they ran in and are also counted separately (`subagentTokens`). A harness
//! whose records carry no token usage reports `null` tokens, never zeros.
use crate::usage::Acc;
use crate::util::{now_ms, LockExt};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Bucket {
    pub input: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub output: i64,
    pub cost: f64,
    /// model replies
    pub turns: u64,
    pub user_msgs: u64,
    pub tool_calls: u64,
    /// tokens (input + cache + output) from subagent files, included in the fields above
    pub subagent_tokens: i64,
    pub model: Option<String>,
}

/// Start of the local clock hour holding `ts` (ms), with the UTC offset in effect at `ts`.
pub fn hour_start(ts: i64, offset_secs: i64) -> i64 {
    let local = ts.div_euclid(1000) + offset_secs;
    (local.div_euclid(3600) * 3600 - offset_secs) * 1000
}

fn local_offset(ts: i64) -> i64 {
    use chrono::{Offset, TimeZone};
    chrono::Local.timestamp_millis_opt(ts).single().map(|d| d.offset().fix().local_minus_utc() as i64).unwrap_or(0)
}

fn ts_of(v: &Value) -> Option<i64> {
    v["timestamp"].as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.timestamp_millis()).or_else(|| v["timestamp"].as_i64())
}

/// Model replies and tool calls in one record (the usage parser counts user messages).
fn replies_and_tools(kind: &str, v: &Value) -> (u64, u64) {
    let count = |items: &Value, ty: &str| items.as_array().map_or(0, |a| a.iter().filter(|x| x["type"] == ty).count() as u64);
    match kind {
        "omp" if v["message"]["role"] == "assistant" => (1, count(&v["message"]["content"], "toolCall")),
        "claude" if v["type"] == "assistant" => (1, count(&v["message"]["content"], "tool_use")),
        "codex" => match (v["type"].as_str(), v["payload"]["type"].as_str()) {
            (Some("event_msg"), Some("agent_message")) => (1, 0),
            (Some("response_item"), Some("function_call" | "custom_tool_call" | "local_shell_call")) => (0, 1),
            _ => (0, 0),
        },
        _ => (0, 0),
    }
}

/// Add one session file's records to `out` (hour → bucket). Returns whether any record carried
/// token usage.
fn feed_file(kind: &str, path: &Path, sub: bool, out: &mut BTreeMap<i64, Bucket>, offset: &dyn Fn(i64) -> i64) -> bool {
    let Ok(f) = std::fs::File::open(path) else { return false };
    let mut acc = Acc::default();
    let mut any_usage = false;
    let mut last_ts = None;
    for line in BufReader::with_capacity(1 << 20, f).split(b'\n').map_while(Result::ok) {
        let Ok(v) = serde_json::from_slice::<Value>(&line) else { continue };
        let Some(ts) = ts_of(&v).or(last_ts) else { continue };
        last_ts = Some(ts);
        let before = acc.clone();
        if let Some(h) = crate::harness::get(kind) {
            h.feed_usage(&v, &mut acc);
        }
        let (replies, tools) = replies_and_tools(kind, &v);
        let d = |a: i64, b: i64| a - b;
        let (di, dcr, dcw, dout) =
            (d(acc.input, before.input), d(acc.cache_read, before.cache_read), d(acc.cache_write, before.cache_write), d(acc.output, before.output));
        let dcost = acc.cost - before.cost;
        let dusers = acc.turns.saturating_sub(before.turns);
        if di == 0 && dcr == 0 && dcw == 0 && dout == 0 && dusers == 0 && replies == 0 && tools == 0 {
            continue;
        }
        if di != 0 || dcr != 0 || dcw != 0 || dout != 0 {
            any_usage = true;
        }
        let b = out.entry(hour_start(ts, offset(ts))).or_default();
        b.input += di;
        b.cache_read += dcr;
        b.cache_write += dcw;
        b.output += dout;
        b.cost += dcost;
        b.turns += replies;
        b.tool_calls += tools;
        if !sub {
            b.user_msgs += dusers;
            if let Some(m) = &acc.model {
                b.model = Some(m.clone());
            }
        } else {
            b.subagent_tokens += di + dcr + dcw + dout;
        }
    }
    any_usage
}

/// Subagent session files of a session: `<session without .jsonl>/**/*.jsonl` (omp) and
/// `<session dir>/<session id>/subagents/*.jsonl` (Claude Code).
fn subagent_files(path: &Path) -> Vec<PathBuf> {
    let dir = path.with_extension("");
    let mut out = vec![];
    fn walk(d: &Path, out: &mut Vec<PathBuf>, depth: usize) {
        if depth > 4 {
            return;
        }
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out, depth + 1);
            } else if p.extension().is_some_and(|x| x == "jsonl") {
                out.push(p);
            }
        }
    }
    walk(&dir, &mut out, 0);
    out.sort();
    out
}

/// Antigravity keeps no token usage: model replies, user messages and tool calls per hour.
fn feed_agy(db: &Path, out: &mut BTreeMap<i64, Bucket>, offset: &dyn Fn(i64) -> i64) {
    for s in crate::agy::read_steps(db, -1).unwrap_or_default() {
        if s.ts <= 0 {
            continue;
        }
        let b = out.entry(hour_start(s.ts, offset(s.ts))).or_default();
        match s.kind {
            crate::agy::USER => b.user_msgs += 1,
            crate::agy::MODEL => b.turns += 1,
            crate::agy::TOOL => b.tool_calls += 1,
            _ => {}
        }
    }
}

/// Buckets of one session plus whether its harness records token usage. Cached per file set
/// by modification times.
pub fn buckets(kind: &str, path: &Path) -> (BTreeMap<i64, Bucket>, bool) {
    type Cached = (String, BTreeMap<i64, Bucket>, bool);
    static CACHE: LazyLock<Mutex<HashMap<PathBuf, Cached>>> = LazyLock::new(Default::default);
    let subs = if kind == "antigravity" { vec![] } else { subagent_files(path) };
    let sig: String = std::iter::once(path)
        .chain(subs.iter().map(PathBuf::as_path))
        .map(|p| format!("{}:{}", p.display(), std::fs::metadata(p).map(|m| m.len()).unwrap_or(0) as i64 + crate::util::mtime_ms(p)))
        .collect::<Vec<_>>()
        .join("|");
    if let Some(c) = CACHE.plock().get(path).filter(|c| c.0 == sig) {
        return (c.1.clone(), c.2);
    }
    let mut out = BTreeMap::new();
    let has_tokens = if kind == "antigravity" {
        feed_agy(path, &mut out, &local_offset);
        false
    } else {
        let mut any = feed_file(kind, path, false, &mut out, &local_offset);
        for s in &subs {
            any |= feed_file(kind, s, true, &mut out, &local_offset);
        }
        // a harness that records tokens, even if this session has not used any yet
        any || matches!(kind, "omp" | "claude" | "codex")
    };
    let mut c = CACHE.plock();
    if c.len() > 64 {
        c.clear();
    }
    c.insert(path.to_path_buf(), (sig, out.clone(), has_tokens));
    (out, has_tokens)
}

/// The API view: the last `hours` hours (every hour present, empty ones zero) and totals.
pub fn view(kind: &str, path: &Path, hours: i64) -> Value {
    let (b, has_tokens) = buckets(kind, path);
    let now = now_ms();
    let end = hour_start(now, local_offset(now));
    let start = end - (hours.clamp(1, 24 * 14) - 1) * 3_600_000;
    let tok = |x: i64| if has_tokens { json!(x) } else { Value::Null };
    let row = |h: i64, x: &Bucket| {
        json!({
            "hourStart": h, "input": tok(x.input), "cacheRead": tok(x.cache_read), "cacheWrite": tok(x.cache_write), "output": tok(x.output),
            "costUsd": if has_tokens { json!((x.cost * 10_000.0).round() / 10_000.0) } else { Value::Null },
            "turns": x.turns, "userMsgs": x.user_msgs, "toolCalls": x.tool_calls, "subagentTokens": tok(x.subagent_tokens), "model": x.model,
        })
    };
    let mut rows = vec![];
    let mut total = Bucket::default();
    let mut h = start;
    while h <= end {
        let x = b.get(&h).cloned().unwrap_or_default();
        total.input += x.input;
        total.cache_read += x.cache_read;
        total.cache_write += x.cache_write;
        total.output += x.output;
        total.cost += x.cost;
        total.turns += x.turns;
        total.user_msgs += x.user_msgs;
        total.tool_calls += x.tool_calls;
        total.subagent_tokens += x.subagent_tokens;
        rows.push(row(h, &x));
        // the next local hour (DST changes keep hours on the clock)
        h = hour_start(h + 3_600_000 + 1, local_offset(h + 3_600_000 + 1));
    }
    let started = b.keys().next().copied();
    json!({
        "hours": rows, "total": row(start, &total), "sessionStartedAt": started, "tokens": has_tokens,
        "from": start, "to": end + 3_600_000,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_lines(kind: &str, lines: &[&str], offset: i64) -> (BTreeMap<i64, Bucket>, bool) {
        let dir = std::env::temp_dir().join(format!("adash-hourly-{}-{}", kind, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s.jsonl");
        std::fs::write(&p, lines.join("\n") + "\n").unwrap();
        let mut out = BTreeMap::new();
        let any = feed_file(kind, &p, false, &mut out, &|_| offset);
        let _ = std::fs::remove_dir_all(&dir);
        (out, any)
    }

    #[test]
    fn hour_boundaries_follow_the_local_clock() {
        // 2026-11-03T00:59:59.999Z and 01:00:00Z fall in different UTC hours
        let a = 1_793_667_599_999;
        assert_eq!(hour_start(a, 0), 1_793_664_000_000);
        assert_eq!(hour_start(a + 1, 0), 1_793_667_600_000);
        // KST (+9 h) hours start at the same instants; a +5:30 zone starts at :30 UTC
        assert_eq!(hour_start(a, 9 * 3600), 1_793_664_000_000);
        assert_eq!(hour_start(a, 5 * 3600 + 1800), 1_793_665_800_000);
        assert_eq!(hour_start(-1, 0), -3_600_000, "floor, not truncation");
    }

    #[test]
    fn omp_records_land_in_their_hours() {
        let (b, any) = feed_lines(
            "omp",
            &[
                r#"{"type":"message","timestamp":"2026-11-03T00:10:00Z","message":{"role":"user","content":[]}}"#,
                r#"{"type":"message","timestamp":"2026-11-03T00:59:59Z","message":{"role":"assistant","model":"m1","content":[{"type":"toolCall"},{"type":"text"}],"usage":{"input":4,"output":10,"cacheRead":100,"cacheWrite":20,"cost":{"total":0.5}}}}"#,
                r#"{"type":"message","timestamp":"2026-11-03T01:00:00Z","message":{"role":"assistant","model":"m2","content":[],"usage":{"input":1,"output":5,"cacheRead":300,"cacheWrite":0,"cost":{"total":0.25}}}}"#,
            ],
            0,
        );
        assert!(any);
        let h0 = hour_start(1_793_664_000_000, 0);
        let x = &b[&h0];
        assert_eq!((x.input, x.output, x.cache_read, x.cache_write, x.turns, x.user_msgs, x.tool_calls), (4, 10, 100, 20, 1, 1, 1));
        assert_eq!(x.model.as_deref(), Some("m1"));
        let y = &b[&(h0 + 3_600_000)];
        assert_eq!((y.input, y.cache_read, y.turns), (1, 300, 1));
        assert!((y.cost - 0.25).abs() < 1e-9);
    }

    #[test]
    fn codex_cumulative_totals_become_hourly_deltas() {
        let tc = |t: &str, i: i64, c: i64, o: i64| {
            format!(
                r#"{{"type":"event_msg","timestamp":"{t}","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{i},"cached_input_tokens":{c},"output_tokens":{o}}}}}}}}}"#
            )
        };
        let l1 = tc("2026-11-03T00:30:00Z", 100, 60, 7);
        let l2 = tc("2026-11-03T01:30:00Z", 250, 160, 20);
        let (b, _) = feed_lines("codex", &[&l1, &l2], 0);
        let h0 = 1_793_664_000_000;
        assert_eq!((b[&h0].input, b[&h0].cache_read, b[&h0].output), (40, 60, 7));
        assert_eq!((b[&(h0 + 3_600_000)].input, b[&(h0 + 3_600_000)].cache_read, b[&(h0 + 3_600_000)].output), (50, 100, 13));
    }

    #[test]
    fn harnesses_without_usage_report_null_tokens() {
        let dir = std::env::temp_dir().join(format!("adash-hourly-null-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("missing.db");
        let v = view("antigravity", &p, 3);
        assert_eq!(v["hours"].as_array().unwrap().len(), 3);
        assert!(v["total"]["input"].is_null() && v["total"]["costUsd"].is_null() && v["tokens"] == false);
        assert_eq!(v["total"]["turns"], 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn subagent_tokens_add_to_the_parent_and_are_counted_apart() {
        let dir = std::env::temp_dir().join(format!("adash-hourly-sub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("s/task")).unwrap();
        let a = r#"{"type":"message","timestamp":"2026-11-03T00:10:00Z","message":{"role":"assistant","content":[],"usage":{"input":1,"output":2,"cacheRead":3,"cacheWrite":4,"cost":{"total":0.1}}}}"#;
        std::fs::write(dir.join("s.jsonl"), format!("{a}\n")).unwrap();
        std::fs::write(dir.join("s/task/0-Scout.jsonl"), format!("{a}\n")).unwrap();
        let (b, any) = buckets("omp", &dir.join("s.jsonl"));
        assert!(any);
        let x = b.values().next().unwrap();
        assert_eq!((x.input, x.output, x.turns, x.subagent_tokens), (2, 4, 2, 10));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
