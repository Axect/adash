//! One adapter per agent harness. Everything that differs between omp, Claude Code, Codex
//! and Antigravity goes through [`Harness`]; the rest of adash asks [`get`] instead of matching
//! on the harness name.
//!
//! Session formats are not stable APIs. When a session file cannot be read as the harness's
//! format, adash logs it once per harness and reason ([`degrade`]) and shows the terminal
//! screen instead.
use crate::ask::Ask;
use crate::session::{ErrInfo, Proc, TurnInfo};
use crate::transcript::Msg;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// How an agent stopped by a provider error is retried.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Retry {
    /// a key sequence the harness binds to "retry the last failed turn"
    Key(&'static str, &'static str),
    /// no retry key: send this message
    Message(&'static str),
}

pub const CONTINUE: &str = "계속 진행해";

pub trait Harness: Sync {
    /// "omp" | "claude" | "codex" | "antigravity"
    fn kind(&self) -> &'static str;
    /// Versions whose session format adash was tested against.
    fn tested_versions(&self) -> &'static str;
    /// Is this command line the harness's main process?
    fn detect(&self, args: &[String]) -> bool;
    /// Label of the transcript source in API responses.
    fn transcript_source(&self) -> &'static str;
    /// Session file of a running process when the process itself says (open file, flags);
    /// `None` leaves it to the generic matching. `bool`: exact.
    fn session_of(&self, p: &Proc) -> Option<(PathBuf, bool)>;
    /// Root directories of the harness's session files.
    fn session_roots(&self) -> Vec<PathBuf>;
    /// Append the chat messages of one session record.
    fn parse_record(&self, r: &Value, n: usize, out: &mut Vec<Msg>);
    /// A provider error that ended the turn, from one session record.
    fn record_error(&self, v: &Value, ts: i64) -> Option<ErrInfo>;
    /// Turn state from one session line.
    fn feed_turn(&self, line: &str, t: &mut TurnInfo) {
        crate::session::feed_turn(self.kind(), line, t)
    }
    /// Open question from one session record.
    fn feed_ask(&self, r: &Value, open: &mut Option<Ask>) {
        crate::ask::feed(self.kind(), r, open)
    }
    /// Can the dashboard answer the harness's question dialog?
    fn answers_questions(&self) -> bool;
    fn retry(&self) -> Retry;
    /// Input prefixes the harness treats as something other than a message to the model.
    fn input_prefixes(&self) -> Vec<(&'static str, &'static str)>;
    /// Prompt text referencing a file.
    fn mention(&self, path: &str) -> String;
    /// Does the harness read `@path` files into the turn?
    fn reads_mentions(&self) -> bool;
    /// Shell command that resumes `file` with the recorded flags.
    fn resume_command(&self, file: &Path, args: &[String]) -> Option<String>;
    /// Keys that end the harness (text, followed by Enter).
    fn exit_keys(&self) -> Vec<(&'static str, bool)>;
    /// Usage totals from one session record.
    fn feed_usage(&self, v: &Value, acc: &mut crate::usage::Acc);
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Flags worth keeping when an agent is started again (value flags).
const KEEP: &[&str] = &["--model", "-m", "--config", "--thinking", "--effort", "--profile", "-c"];

fn kept_flags(args: &[String]) -> String {
    let mut out = vec![];
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        if let Some((f, _)) = a.split_once('=') {
            if KEEP.contains(&f) {
                out.push(shell_quote(a));
            }
        } else if KEEP.contains(&a.as_str()) && i + 1 < args.len() {
            out.push(shell_quote(a));
            out.push(shell_quote(&args[i + 1]));
            i += 1;
        }
        i += 1;
    }
    if out.is_empty() {
        String::new()
    } else {
        format!(" {}", out.join(" "))
    }
}

fn base(s: &str) -> String {
    Path::new(s).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
}

fn stem(file: &Path) -> String {
    file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn at_mention(path: &str) -> String {
    if path.chars().any(char::is_whitespace) {
        format!("@\"{path}\"")
    } else {
        format!("@{path}")
    }
}

fn plain_mention(path: &str) -> String {
    if path.chars().any(char::is_whitespace) {
        format!("\"{path}\"")
    } else {
        path.to_string()
    }
}

// ---------------------------------------------------------------------------------------------

pub struct Omp;

impl Harness for Omp {
    fn kind(&self) -> &'static str {
        "omp"
    }
    fn tested_versions(&self) -> &'static str {
        "18.13–18.15"
    }
    fn detect(&self, args: &[String]) -> bool {
        let a1 = args.get(1).map(String::as_str).unwrap_or("");
        args.first().is_some_and(|a| base(a) == "omp") && !a1.starts_with("__omp_") && a1 != "browser-relay"
    }
    fn transcript_source(&self) -> &'static str {
        "omp-jsonl"
    }
    // omp sessions are matched by the generic pass (open file, --resume, pins, title, content)
    fn session_of(&self, _p: &Proc) -> Option<(PathBuf, bool)> {
        None
    }
    fn session_roots(&self) -> Vec<PathBuf> {
        vec![crate::util::home().join(".omp/agent/sessions")]
    }
    fn parse_record(&self, r: &Value, _n: usize, out: &mut Vec<Msg>) {
        crate::transcript::parse_omp(r, out)
    }
    fn record_error(&self, v: &Value, ts: i64) -> Option<ErrInfo> {
        let m = &v["message"];
        if m["role"] != "assistant" || m["stopReason"] != "error" {
            return None;
        }
        let raw = m["errorMessage"].as_str().unwrap_or("error");
        Some(ErrInfo {
            status: m["errorStatus"].as_i64().or_else(|| crate::session::status_in(raw)),
            message: crate::session::error_summary(raw),
            provider: m["provider"].as_str().map(String::from),
            model: m["model"].as_str().map(String::from),
            at: ts,
        })
    }
    fn answers_questions(&self) -> bool {
        true
    }
    // `app.retry` ("Retry last failed assistant turn") is bound to F5 and Alt+R
    fn retry(&self) -> Retry {
        Retry::Key("\x1b[15~", "F5")
    }
    fn input_prefixes(&self) -> Vec<(&'static str, &'static str)> {
        vec![("!", "shell"), ("$ ", "python")]
    }
    fn mention(&self, path: &str) -> String {
        at_mention(path)
    }
    fn reads_mentions(&self) -> bool {
        true
    }
    fn resume_command(&self, file: &Path, args: &[String]) -> Option<String> {
        Some(format!("omp{} --resume {}", kept_flags(args), shell_quote(&file.to_string_lossy())))
    }
    fn exit_keys(&self) -> Vec<(&'static str, bool)> {
        vec![("/exit", true)]
    }
    fn feed_usage(&self, v: &Value, acc: &mut crate::usage::Acc) {
        crate::usage::feed_omp(v, acc)
    }
}

pub struct Claude;

impl Harness for Claude {
    fn kind(&self) -> &'static str {
        "claude"
    }
    fn tested_versions(&self) -> &'static str {
        "2.1"
    }
    fn detect(&self, args: &[String]) -> bool {
        let a1 = args.get(1).map(String::as_str).unwrap_or("");
        match args.first().map(|a| base(a)).as_deref() {
            Some("claude") => true,
            Some("node" | "bun") => matches!(base(a1).as_str(), "claude" | "cli.js") && a1.contains("claude"),
            _ => false,
        }
    }
    fn transcript_source(&self) -> &'static str {
        "claude-jsonl"
    }
    fn session_of(&self, p: &Proc) -> Option<(PathBuf, bool)> {
        crate::session::claude_file(p)
    }
    fn session_roots(&self) -> Vec<PathBuf> {
        vec![crate::util::home().join(".claude/projects")]
    }
    fn parse_record(&self, r: &Value, _n: usize, out: &mut Vec<Msg>) {
        crate::transcript::parse_claude(r, out)
    }
    fn record_error(&self, v: &Value, ts: i64) -> Option<ErrInfo> {
        if v["type"] != "assistant" || v["isApiErrorMessage"] != true {
            return None;
        }
        let raw = match &v["message"]["content"] {
            Value::Array(a) => a.iter().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n"),
            Value::String(s) => s.clone(),
            _ => v["error"].as_str().unwrap_or("API error").to_string(),
        };
        Some(ErrInfo {
            status: v["apiErrorStatus"].as_i64().or_else(|| crate::session::status_in(&raw)),
            message: crate::session::error_summary(&raw),
            provider: Some("anthropic".into()),
            model: v["message"]["model"].as_str().filter(|m| *m != "<synthetic>").map(String::from),
            at: ts,
        })
    }
    fn answers_questions(&self) -> bool {
        true
    }
    fn retry(&self) -> Retry {
        Retry::Message(CONTINUE)
    }
    fn input_prefixes(&self) -> Vec<(&'static str, &'static str)> {
        vec![("!", "shell"), ("#", "memory")]
    }
    fn mention(&self, path: &str) -> String {
        at_mention(path)
    }
    fn reads_mentions(&self) -> bool {
        true
    }
    fn resume_command(&self, file: &Path, args: &[String]) -> Option<String> {
        Some(format!("claude{} --resume {}", kept_flags(args), shell_quote(&stem(file))))
    }
    fn exit_keys(&self) -> Vec<(&'static str, bool)> {
        vec![("/exit", true)]
    }
    fn feed_usage(&self, v: &Value, acc: &mut crate::usage::Acc) {
        crate::usage::feed_claude(v, acc)
    }
}

pub struct Codex;

impl Harness for Codex {
    fn kind(&self) -> &'static str {
        "codex"
    }
    fn tested_versions(&self) -> &'static str {
        "0.16x"
    }
    fn detect(&self, args: &[String]) -> bool {
        let a1 = args.get(1).map(String::as_str).unwrap_or("");
        match args.first().map(|a| base(a)).as_deref() {
            Some("codex") => true,
            Some("node" | "bun") => matches!(base(a1).as_str(), "codex" | "codex.js"),
            _ => false,
        }
    }
    fn transcript_source(&self) -> &'static str {
        "codex-rollout"
    }
    fn session_of(&self, p: &Proc) -> Option<(PathBuf, bool)> {
        crate::session::codex_file(p)
    }
    fn session_roots(&self) -> Vec<PathBuf> {
        vec![crate::util::home().join(".codex/sessions")]
    }
    fn parse_record(&self, r: &Value, n: usize, out: &mut Vec<Msg>) {
        crate::transcript::parse_codex(r, n, out)
    }
    fn record_error(&self, v: &Value, ts: i64) -> Option<ErrInfo> {
        let p = &v["payload"];
        if v["type"] != "event_msg" || p["type"] != "error" {
            return None;
        }
        let raw = p["message"].as_str().unwrap_or("error");
        let status = crate::session::status_in(raw).or(match p["codex_error_info"].as_str() {
            Some("unauthorized") => Some(401),
            Some("usage_limit_exceeded") => Some(429),
            _ => None,
        });
        Some(ErrInfo { status, message: crate::session::error_summary(raw), provider: Some("openai".into()), model: None, at: ts })
    }
    fn answers_questions(&self) -> bool {
        false
    }
    fn retry(&self) -> Retry {
        Retry::Message(CONTINUE)
    }
    fn input_prefixes(&self) -> Vec<(&'static str, &'static str)> {
        vec![("!", "shell")]
    }
    fn mention(&self, path: &str) -> String {
        plain_mention(path)
    }
    fn reads_mentions(&self) -> bool {
        false
    }
    // rollout files end with the session uuid
    fn resume_command(&self, file: &Path, args: &[String]) -> Option<String> {
        let s = stem(file);
        let id = if s.len() >= 36 { s[s.len() - 36..].to_string() } else { s };
        Some(format!("codex{} resume {}", kept_flags(args), shell_quote(&id)))
    }
    fn exit_keys(&self) -> Vec<(&'static str, bool)> {
        vec![("/quit", true)]
    }
    fn feed_usage(&self, v: &Value, acc: &mut crate::usage::Acc) {
        crate::usage::feed_codex(v, acc)
    }
}

pub struct Antigravity;

impl Harness for Antigravity {
    fn kind(&self) -> &'static str {
        "antigravity"
    }
    fn tested_versions(&self) -> &'static str {
        "1.4"
    }
    fn detect(&self, args: &[String]) -> bool {
        args.first().is_some_and(|a| matches!(base(a).as_str(), "agy" | "antigravity"))
    }
    fn transcript_source(&self) -> &'static str {
        "antigravity-db"
    }
    fn session_of(&self, p: &Proc) -> Option<(PathBuf, bool)> {
        crate::agy::conversation_for_pid(p.pid).map(|f| (f, true))
    }
    fn session_roots(&self) -> Vec<PathBuf> {
        vec![crate::agy::root().join("conversations")]
    }
    // conversations are SQLite steps, decoded by `agy` / `TranscriptCache::agy_messages`
    fn parse_record(&self, _r: &Value, _n: usize, _out: &mut Vec<Msg>) {}
    fn record_error(&self, _v: &Value, _ts: i64) -> Option<ErrInfo> {
        None
    }
    fn feed_turn(&self, _line: &str, _t: &mut TurnInfo) {}
    fn feed_ask(&self, _r: &Value, _open: &mut Option<Ask>) {}
    fn answers_questions(&self) -> bool {
        false
    }
    fn retry(&self) -> Retry {
        Retry::Message(CONTINUE)
    }
    fn input_prefixes(&self) -> Vec<(&'static str, &'static str)> {
        vec![]
    }
    fn mention(&self, path: &str) -> String {
        plain_mention(path)
    }
    fn reads_mentions(&self) -> bool {
        false
    }
    fn resume_command(&self, file: &Path, _args: &[String]) -> Option<String> {
        Some(format!("agy --conversation {}", shell_quote(&stem(file))))
    }
    // no exit command: Ctrl+C twice
    fn exit_keys(&self) -> Vec<(&'static str, bool)> {
        vec![("\u{3}", false), ("\u{3}", false)]
    }
    fn feed_usage(&self, _v: &Value, _acc: &mut crate::usage::Acc) {}
}

static ALL: [&dyn Harness; 4] = [&Omp, &Claude, &Codex, &Antigravity];

/// Every harness adash knows (enabled or not).
pub fn all() -> &'static [&'static dyn Harness] {
    &ALL
}

/// The adapter for a harness name.
pub fn get(kind: &str) -> Option<&'static dyn Harness> {
    ALL.iter().copied().find(|h| h.kind() == kind)
}

/// The enabled harness whose main process this command line is.
pub fn detect(args: &[String]) -> Option<&'static dyn Harness> {
    if args.is_empty() {
        return None;
    }
    ALL.iter().copied().find(|h| h.detect(args) && crate::config::get().harness_enabled(h.kind()))
}

/// Log once per harness and reason that a session could not be read as expected; callers
/// then fall back to the terminal screen.
pub fn degrade(kind: &str, reason: &str) {
    use std::sync::{LazyLock, Mutex};
    static SEEN: LazyLock<Mutex<std::collections::HashSet<String>>> = LazyLock::new(Default::default);
    let key = format!("{kind}: {reason}");
    if SEEN.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone()) {
        eprintln!("adash: {key} (showing the terminal instead; tested versions: {})", get(kind).map(|h| h.tested_versions()).unwrap_or("?"));
    }
}

#[cfg(test)]
mod tests {
    //! Contract tests: a small synthetic session per harness through the adapter.
    use super::*;

    fn lines(s: &str) -> Vec<Value> {
        s.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn transcript(h: &dyn Harness, recs: &[Value]) -> Vec<Msg> {
        let mut out = vec![];
        for (n, r) in recs.iter().enumerate() {
            h.parse_record(r, n, &mut out);
        }
        out
    }

    #[test]
    fn detection_by_command_line() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(detect(&a(&["/usr/bin/omp", "--model", "x"])).map(|h| h.kind()), Some("omp"));
        assert!(detect(&a(&["omp", "__omp_worker"])).is_none());
        assert_eq!(detect(&a(&["node", "/opt/lib/claude/cli.js"])).map(|h| h.kind()), Some("claude"));
        assert_eq!(detect(&a(&["codex", "resume", "x"])).map(|h| h.kind()), Some("codex"));
        assert_eq!(detect(&a(&["agy"])).map(|h| h.kind()), Some("antigravity"));
        assert!(detect(&a(&["bash"])).is_none());
    }

    #[test]
    fn omp_contract() {
        let h = get("omp").unwrap();
        let recs = lines(
            r#"
{"type":"message","id":"u1","timestamp":"2026-10-01T00:00:00Z","message":{"role":"user","content":[{"type":"text","text":"Plot the survey footprint"}]}}
{"type":"message","id":"a1","timestamp":"2026-10-01T00:00:05Z","message":{"role":"assistant","provider":"anthropic","model":"claude-opus-5-5","content":[{"type":"text","text":"Done: footprint.png"}],"stopReason":"stop","usage":{"input":10,"output":20,"cacheRead":900,"cacheWrite":5,"cost":{"total":0.1}}}}
{"type":"message","id":"b1","timestamp":"2026-10-01T00:00:06Z","message":{"role":"bashExecution","command":"ls","output":"a\n","exitCode":0}}
{"type":"message","id":"e1","timestamp":"2026-10-01T00:00:09Z","message":{"role":"assistant","provider":"anthropic","model":"claude-opus-5-5","content":[],"stopReason":"error","errorStatus":429,"errorMessage":"429 {\"error\":{\"message\":\"Rate limited\"}}"}}
"#,
        );
        let msgs = transcript(h, &recs);
        let roles: Vec<&str> = msgs.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["user", "assistant", "shell", "error"]);
        let e = h.record_error(&recs[3], 9).unwrap();
        assert_eq!((e.status, e.message.as_str()), (Some(429), "Rate limited"));
        let mut t = TurnInfo::default();
        for r in &recs {
            h.feed_turn(&r.to_string(), &mut t);
        }
        assert!(t.error.is_some() && t.last_human.is_some());
        let mut acc = crate::usage::Acc::default();
        for r in &recs {
            h.feed_usage(r, &mut acc);
        }
        assert_eq!((acc.turns, acc.cache_read), (1, 900));
        assert_eq!(h.retry(), Retry::Key("\x1b[15~", "F5"));
        assert_eq!(h.mention("a b.md"), "@\"a b.md\"");
        assert!(h.resume_command(Path::new("/s/x.jsonl"), &[]).unwrap().ends_with("--resume '/s/x.jsonl'"));
    }

    #[test]
    fn claude_contract() {
        let h = get("claude").unwrap();
        let recs = lines(
            r#"
{"type":"user","uuid":"u1","timestamp":"2026-10-01T00:00:00Z","message":{"role":"user","content":"Summarize the fold energies"}}
{"type":"assistant","uuid":"a1","timestamp":"2026-10-01T00:00:04Z","message":{"model":"claude-haiku-5-5","content":[{"type":"text","text":"Three folds are stable."}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":7,"cache_read_input_tokens":500,"cache_creation_input_tokens":40}}}
{"type":"assistant","uuid":"x1","timestamp":"2026-10-01T00:00:08Z","isApiErrorMessage":true,"message":{"model":"<synthetic>","content":[{"type":"text","text":"API Error: 529 {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}"}]}}
"#,
        );
        let msgs = transcript(h, &recs);
        assert_eq!(msgs.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(), ["user", "assistant", "error"]);
        let e = h.record_error(&recs[2], 8).unwrap();
        assert_eq!((e.status, e.message.as_str(), e.model), (Some(529), "Overloaded", None));
        let mut acc = crate::usage::Acc::default();
        for r in &recs {
            h.feed_usage(r, &mut acc);
        }
        assert_eq!((acc.turns, acc.cache_write), (1, 40));
        assert_eq!(h.retry(), Retry::Message(CONTINUE));
        assert_eq!(h.resume_command(Path::new("/p/0f1e.jsonl"), &[]).unwrap(), "claude --resume '0f1e'");
    }

    #[test]
    fn codex_contract() {
        let h = get("codex").unwrap();
        let recs = lines(
            r#"
{"timestamp":"2026-10-01T00:00:00Z","type":"event_msg","payload":{"type":"user_message","message":"Retrain the climate emulator"}}
{"timestamp":"2026-10-01T00:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Retrain the climate emulator"}]}}
{"timestamp":"2026-10-01T00:00:05Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Started run 12."}]}}
{"timestamp":"2026-10-01T00:00:06Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":60,"output_tokens":7},"last_token_usage":{"input_tokens":40,"output_tokens":3},"model_context_window":258400},"rate_limits":{"primary":{"used_percent":12.0,"window_minutes":10080,"resets_at":1793862443}}}}
{"timestamp":"2026-10-01T00:00:09Z","type":"event_msg","payload":{"type":"error","message":"stream disconnected","codex_error_info":"unauthorized"}}
"#,
        );
        let msgs = transcript(h, &recs);
        assert_eq!(msgs.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(), ["user", "assistant", "error"]);
        assert_eq!(h.record_error(&recs[4], 9).unwrap().status, Some(401));
        let mut acc = crate::usage::Acc::default();
        for r in &recs {
            h.feed_usage(r, &mut acc);
        }
        assert_eq!((acc.cache_read, acc.window), (60, Some(258400)));
        assert_eq!(h.mention("web/app.js"), "web/app.js");
        assert!(h
            .resume_command(Path::new("/c/rollout-2026-10-01T00-00-00-01a18e7b-14de-72af-bfcf-dfd164b211eb.jsonl"), &[])
            .unwrap()
            .ends_with("resume '01a18e7b-14de-72af-bfcf-dfd164b211eb'"));
    }

    #[test]
    fn antigravity_contract() {
        // conversations are SQLite (decoded in `agy`, see its tests); the adapter covers the rest
        let h = get("antigravity").unwrap();
        assert!(transcript(h, &[serde_json::json!({"type": "message"})]).is_empty());
        assert!(!h.answers_questions() && h.input_prefixes().is_empty());
        assert_eq!(h.exit_keys().len(), 2);
        assert_eq!(h.resume_command(Path::new("/c/abc.db"), &[]).unwrap(), "agy --conversation 'abc'");
    }

    #[test]
    fn every_harness_declares_what_it_was_tested_with() {
        for h in all() {
            assert!(!h.tested_versions().is_empty(), "{}", h.kind());
            assert!(!h.transcript_source().is_empty());
        }
    }
}
