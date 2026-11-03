//! /proc scan (process → adash terminal handle, env `ADASH_HANDLE`) and READ-ONLY
//! session-file parsing for omp / claude / codex progress.
use crate::util::{home, mtime_ms, omp_file_created_ms};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

#[derive(Clone, Debug)]
pub struct Proc {
    pub pid: u32,
    pub kind: String, // omp | antigravity | claude | codex
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub start_ms: i64,
    pub handle: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub current: Option<String>,
    pub source: String,
}

#[derive(Clone, Debug, Default)]
pub struct SessionInfo {
    pub path: Option<PathBuf>,
    pub exact: bool,
    /// "exact" (fd / --resume / claude-codex fd), "title" (unique title match), "heuristic"
    pub confidence: &'static str,
    pub title: Option<String>,
    pub model: Option<String>,
    pub progress: Option<Progress>,
    /// unanswered interactive question in the session (omp `ask`, claude `AskUserQuestion`)
    pub ask: Option<crate::ask::Ask>,
    /// from the session record: turn phase, last human message, last finished answer
    pub turn: TurnInfo,
}

pub fn ppid(pid: u32) -> Option<u32> {
    crate::platform::ppid(pid)
}

pub fn env_of(pid: u32, key: &str) -> Option<String> {
    crate::platform::env_var(pid, key)
}

fn classify(args: &[String]) -> Option<&'static str> {
    crate::harness::detect(args).map(|h| h.kind())
}

/// All agent main processes running inside adash terminals (env `ADASH_HANDLE`), grouped by handle.
pub fn scan_procs() -> HashMap<String, Vec<Proc>> {
    let mut out: HashMap<String, Vec<Proc>> = HashMap::new();
    for pid in crate::platform::pids() {
        let Some(args) = crate::platform::args(pid) else { continue };
        let Some(kind) = classify(&args) else { continue };
        let Some(handle) = env_of(pid, crate::term::HANDLE_ENV).filter(|h| !h.is_empty()) else { continue };
        let cwd = crate::platform::cwd(pid).unwrap_or_default();
        out.entry(handle.clone()).or_default().push(Proc { pid, kind: kind.into(), args, cwd, start_ms: crate::platform::start_ms(pid), handle });
    }
    for v in out.values_mut() {
        v.sort_by_key(|p| p.start_ms);
    }
    out
}

/// The agent process's working directory (its session cwd).
pub fn proc_cwd(a: &crate::collect::Agent) -> Option<PathBuf> {
    let h = a.handle.as_deref()?;
    scan_procs().remove(h)?.into_iter().find(|p| p.kind == a.agent_type).map(|p| p.cwd)
}

/// Some agent process on this machine (inside adash or not, e.g. a terminal of another app)
/// holds the session: it was started on it (`--resume <file>`, `resume <id>`, ...) or has the
/// file open. A second writer would corrupt the session.
pub fn procs_using_session(file: &Path, id: &str) -> bool {
    let fs = file.to_string_lossy().into_owned();
    crate::platform::pids().into_iter().any(|pid| {
        let Some(args) = crate::platform::args(pid) else { return false };
        classify(&args).is_some() && (args.iter().any(|x| *x == fs || (!id.is_empty() && *x == id)) || fd_match(pid, |l| l == fs).is_some())
    })
}

pub fn arg_value(args: &[String], flag: &str) -> Option<String> {
    let eq = format!("{flag}=");
    for (i, a) in args.iter().enumerate() {
        if a == flag {
            return args.get(i + 1).cloned();
        }
        if let Some(v) = a.strip_prefix(&eq) {
            return Some(v.to_string());
        }
    }
    None
}

// ---------- incremental file parsing ----------

#[derive(Default)]
struct FileCache {
    offset: u64,
    /// agy: highest step idx seen (`offset` is where the next read starts; it can stay behind
    /// this while a step is still running)
    seen: u64,
    len: u64,
    model: Option<String>,
    progress: Option<Progress>,
    ask: Option<crate::ask::Ask>,
    turn: TurnInfo,
}

#[derive(Default)]
pub struct SessionCache {
    files: HashMap<PathBuf, FileCache>,
    /// handle → (pid, process start ms, session file, confidence): kept while the process
    /// lives, persisted in `session_pins.json` so a restart of adash does not lose it
    sticky: HashMap<String, (u32, i64, PathBuf, &'static str)>,
    pins_loaded: bool,
}

fn pins_path() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join(format!("adash-test-pins-{}.json", std::process::id()));
    }
    crate::util::state_dir().join("session_pins.json")
}

fn conf_static(c: &str) -> &'static str {
    match c {
        "exact" => "exact",
        "title" => "title",
        _ => "heuristic",
    }
}

/// The main omp session file for a path under `~/.omp/agent/sessions`: a main file sits right
/// in the project slug folder (`<slug>/<ISO>Z_<uuid>.jsonl`); a subagent file
/// (`<slug>/<main stem>/<Name>.jsonl`, any depth) maps to `<slug>/<main stem>.jsonl`.
pub fn omp_main(path: &Path) -> Option<PathBuf> {
    omp_main_in(&home().join(".omp/agent/sessions"), path)
}

fn omp_main_in(root: &Path, path: &Path) -> Option<PathBuf> {
    let rel = path.strip_prefix(root).ok()?;
    let comps: Vec<_> = rel.components().collect();
    match comps.len() {
        0 | 1 => None,
        2 => Some(path.to_path_buf()),
        _ => {
            let main = root.join(comps[0].as_os_str()).join(format!("{}.jsonl", comps[1].as_os_str().to_string_lossy()));
            main.exists().then_some(main)
        }
    }
}

fn load_pins() -> HashMap<String, (u32, i64, PathBuf, &'static str)> {
    let v: Value = std::fs::read_to_string(pins_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
    v.as_object()
        .into_iter()
        .flatten()
        .filter_map(|(h, p)| {
            // a pin left on a subagent file points to its main session
            let f = PathBuf::from(p["path"].as_str()?);
            let f = omp_main(&f).unwrap_or(f);
            Some((h.clone(), (p["pid"].as_u64()? as u32, p["start"].as_i64()?, f, conf_static(p["conf"].as_str()?))))
        })
        .collect()
}

/// Turn bookkeeping from session records.
/// A provider/API error that ended the agent's turn.
#[derive(Default, Clone, Debug, PartialEq, serde::Serialize)]
pub struct ErrInfo {
    pub status: Option<i64>,
    /// one-line summary (the provider's `message` when the text carries JSON)
    pub message: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub at: i64,
}

/// HTTP-like status at the start of an error text ("403 {…}", "API Error: 529 …").
pub(crate) fn status_in(t: &str) -> Option<i64> {
    let re = regex::Regex::new(r"(?:^|API Error:\s*|status(?: code)?:?\s*)([45]\d\d)\b").ok()?;
    re.captures(t.trim()).and_then(|c| c[1].parse().ok())
}

/// "403 {\"error\":{\"message\":\"Request not allowed\"}}" → "Request not allowed".
pub fn error_summary(t: &str) -> String {
    let t = t.trim().trim_start_matches("API Error:").trim();
    if let Some(i) = t.find('{') {
        // the first JSON value (text may follow it)
        if let Some(Ok(v)) = serde_json::Deserializer::from_str(&t[i..]).into_iter::<Value>().next() {
            let m = v["error"]["message"].as_str().or(v["message"].as_str()).or(v["error"].as_str());
            if let Some(m) = m {
                return crate::util::truncate(m, 200);
            }
        }
    }
    let t = t.trim_start_matches(|c: char| c.is_ascii_digit()).trim();
    crate::util::truncate(crate::util::first_line(t, 200).as_str(), 200)
}

/// The error a session record reports, if any: omp assistant `stopReason:"error"`, claude
/// `isApiErrorMessage` assistant records, codex `event_msg` `error`.
pub fn record_error(kind: &str, v: &Value, ts: i64) -> Option<ErrInfo> {
    crate::harness::get(kind)?.record_error(v, ts)
}

#[derive(Default, Clone, Debug, PartialEq)]
pub struct TurnInfo {
    /// (agent mid-turn, since ms)
    pub phase: Option<(bool, i64)>,
    /// last user message typed by a human (not `[adash …]` lines)
    pub last_human: Option<i64>,
    /// last finished agent turn (the agent's final answer)
    pub last_answer: Option<i64>,
    /// the turn ended on a provider error (cleared by the next user message or agent output)
    pub error: Option<ErrInfo>,
    /// latest user message text (the prompt the agent works on)
    pub prompt: Option<String>,
    /// latest assistant text
    pub last_text: Option<String>,
    /// tool the open turn called last (None once the turn ends)
    pub tool: Option<String>,
}

fn user_text(v: &Value, kind: &str) -> String {
    if kind == "omp" {
        if let Some((t, _)) = crate::transcript::omp_user(v) {
            return t;
        }
    }
    let c = match kind {
        "codex" => return v["payload"]["message"].as_str().unwrap_or("").to_string(),
        _ => &v["message"]["content"],
    };
    match c {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter(|x| x["type"] == "text").filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// Turn state from one session line: a user message opens a turn; the agent's final answer
/// (omp `stopReason` other than toolUse, claude `stop_reason` other than tool_use, codex
/// task_complete/turn_aborted) closes it.
pub(crate) fn feed_turn(kind: &str, line: &str, t: &mut TurnInfo) {
    let pre = match kind {
        "omp" => {
            line.contains("\"role\":\"user\"")
                || line.contains("\"role\":\"assistant\"")
                || line.contains("\"skill-prompt\"")
                || line.contains("\"toolName\":\"ask\"")
        }
        "claude" => line.contains("\"type\":\"user\"") || line.contains("\"type\":\"assistant\""),
        "codex" => {
            line.contains("task_started")
                || line.contains("task_complete")
                || line.contains("turn_aborted")
                || line.contains("\"user_message\"")
                || line.contains("\"type\":\"error\"")
                || line.contains("\"agent_message\"")
                || line.contains("\"function_call\"")
                || line.contains("\"custom_tool_call\"")
        }
        _ => false,
    };
    if !pre {
        return;
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else { return };
    let ts = v["timestamp"].as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.timestamp_millis()).unwrap_or(0);
    let tool_result = kind == "claude" && v["message"]["content"].as_array().is_some_and(|a| a.iter().any(|c| c["type"] == "tool_result"));
    let is_user = match kind {
        "omp" => crate::transcript::omp_user(&v).is_some(),
        "claude" => v["type"] == "user" && v["isMeta"] != true && !tool_result,
        "codex" => v["payload"]["type"] == "user_message",
        _ => false,
    };
    // review-script pointer lines come from another agent: not the human's last word
    let ut = user_text(&v, kind);
    // provider error: remembered until the human writes again or the agent produces output
    if let Some(e) = record_error(kind, &v, ts) {
        t.error = Some(e);
    } else if is_user
        || (kind == "omp" && v["message"]["role"] == "assistant")
        || (kind == "claude" && v["type"] == "assistant")
        || (kind == "codex" && matches!(v["payload"]["type"].as_str(), Some("task_started" | "agent_message")))
    {
        t.error = None;
    }
    if is_user && !ut.trim_start().starts_with("[adash ") && !crate::analytics::is_peer_pointer(ut.trim_start()) {
        t.last_human = Some(ts);
    }
    if is_user {
        if !ut.trim().is_empty() {
            t.prompt = Some(ut.clone());
        }
        t.tool = None;
    }
    // assistant text and tool calls: what the agent said last, which tool it is running
    let (text, tool) = match kind {
        "omp" if v["message"]["role"] == "assistant" => {
            let c = v["message"]["content"].as_array();
            (
                c.into_iter().flatten().filter(|x| x["type"] == "text").filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n"),
                c.into_iter().flatten().filter(|x| x["type"] == "toolCall").filter_map(|x| x["name"].as_str()).next_back(),
            )
        }
        "claude" if v["type"] == "assistant" => {
            let c = v["message"]["content"].as_array();
            (
                c.into_iter().flatten().filter(|x| x["type"] == "text").filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n"),
                c.into_iter().flatten().filter(|x| x["type"] == "tool_use").filter_map(|x| x["name"].as_str()).next_back(),
            )
        }
        "codex" => match v["payload"]["type"].as_str() {
            Some("agent_message") => (v["payload"]["message"].as_str().unwrap_or("").to_string(), None),
            Some("function_call" | "custom_tool_call") => (String::new(), v["payload"]["name"].as_str()),
            _ => (String::new(), None),
        },
        _ => (String::new(), None),
    };
    if !text.trim().is_empty() {
        t.last_text = Some(text);
    }
    if let Some(n) = tool {
        t.tool = Some(n.to_string());
    }
    // answering an ask is the human's turn too (the agent's turn stays open)
    if kind == "omp" && v["message"]["role"] == "toolResult" && v["message"]["toolName"] == "ask" && v["message"]["isError"] != true {
        t.last_human = Some(ts);
    }
    let open = match kind {
        "omp" => match v["message"]["role"].as_str() {
            Some("user") => Some(true),
            _ if crate::transcript::omp_user(&v).is_some() => Some(true),
            Some("assistant") => Some(matches!(v["message"]["stopReason"].as_str(), Some("toolUse") | None)),
            _ => None,
        },
        "claude" => match v["type"].as_str() {
            Some("user") if v["isMeta"] != true => Some(true),
            Some("assistant") => Some(v["message"]["stop_reason"].as_str().is_none_or(|r| r == "tool_use")),
            _ => None,
        },
        "codex" => match v["payload"]["type"].as_str() {
            Some("task_started") => Some(true),
            Some("task_complete" | "turn_aborted") => Some(false),
            _ => None,
        },
        _ => None,
    };
    if let Some(o) = open {
        if !o {
            t.last_answer = Some(ts);
            t.tool = None;
        }
        // keep the start of the current phase (a busy turn emits many records)
        if t.phase.is_none_or(|(prev, _)| prev != o) {
            t.phase = Some((o, ts));
        }
    }
}

/// Terminal title vs session title record; the terminal may cut the title with "…".
fn titles_equal(term: &str, sess: &str) -> bool {
    term == sess || term.strip_suffix('…').is_some_and(|t| !t.is_empty() && sess.starts_with(t.trim_end()))
}

static CODEX_STEP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"step:\s*"((?:[^"\\]|\\.)*)"\s*,\s*status:\s*"(\w+)""#).unwrap());

fn tally(items: &[(String, String)], source: &str) -> Option<Progress> {
    let live: Vec<&(String, String)> = items.iter().filter(|(_, s)| !matches!(s.as_str(), "abandoned" | "dropped" | "cancelled" | "removed")).collect();
    if live.is_empty() {
        return None;
    }
    let done = live.iter().filter(|(_, s)| matches!(s.as_str(), "completed" | "complete" | "done")).count();
    let current = live.iter().find(|(_, s)| s == "in_progress" || s == "active").or_else(|| live.iter().find(|(_, s)| s == "pending")).map(|(c, _)| c.clone());
    Some(Progress { done, total: live.len(), current, source: source.into() })
}

fn parse_line(kind: &str, line: &str, fc: &mut FileCache) {
    match kind {
        "omp" => {
            if line.contains("\"model_change\"") {
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    if let Some(m) = v["model"].as_str() {
                        fc.model = Some(m.to_string());
                    }
                }
            } else if line.contains("\"toolName\":\"todo\"") {
                let Ok(v) = serde_json::from_str::<Value>(line) else { return };
                let m = &v["message"];
                if m["role"] != "toolResult" || m["toolName"] != "todo" {
                    return;
                }
                let Some(phases) = m["details"]["phases"].as_array() else { return };
                let items: Vec<(String, String)> = phases
                    .iter()
                    .flat_map(|p| p["tasks"].as_array().cloned().unwrap_or_default())
                    .map(|t| (t["content"].as_str().unwrap_or("").to_string(), t["status"].as_str().unwrap_or("").to_string()))
                    .collect();
                fc.progress = tally(&items, "omp-todo");
            }
        }
        "claude" => {
            if !line.contains("TodoWrite") {
                if line.contains("\"model\"") && line.contains("\"assistant\"") {
                    if let Ok(v) = serde_json::from_str::<Value>(line) {
                        if let Some(m) = v["message"]["model"].as_str() {
                            fc.model = Some(m.to_string());
                        }
                    }
                }
                return;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else { return };
            for c in v["message"]["content"].as_array().cloned().unwrap_or_default() {
                if c["type"] == "tool_use" && c["name"] == "TodoWrite" {
                    let items: Vec<(String, String)> = c["input"]["todos"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .map(|t| (t["content"].as_str().unwrap_or("").to_string(), t["status"].as_str().unwrap_or("").to_string()))
                        .collect();
                    fc.progress = tally(&items, "claude-todo");
                }
            }
            if let Some(m) = v["message"]["model"].as_str() {
                fc.model = Some(m.to_string());
            }
        }
        "codex" => {
            if line.contains("\"turn_context\"") {
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    if let Some(m) = v["payload"]["model"].as_str() {
                        fc.model = Some(m.to_string());
                    }
                }
            }
            if !line.contains("update_plan") {
                return;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else { return };
            let p = &v["payload"];
            let mut items = Vec::new();
            if p["type"] == "function_call" && p["name"] == "update_plan" {
                if let Ok(a) = serde_json::from_str::<Value>(p["arguments"].as_str().unwrap_or("{}")) {
                    for s in a["plan"].as_array().cloned().unwrap_or_default() {
                        items.push((s["step"].as_str().unwrap_or("").to_string(), s["status"].as_str().unwrap_or("").to_string()));
                    }
                }
            } else if p["type"] == "custom_tool_call" {
                let input = p["input"].as_str().unwrap_or("");
                if let Some(idx) = input.rfind("update_plan(") {
                    for c in CODEX_STEP.captures_iter(&input[idx..]) {
                        items.push((c[1].replace("\\\"", "\""), c[2].to_string()));
                    }
                }
            }
            if !items.is_empty() {
                fc.progress = tally(&items, "codex-plan");
            }
        }
        _ => {}
    }
}

impl SessionCache {
    /// Parse new bytes appended since the last call.
    fn update(&mut self, kind: &str, path: &Path) -> (Option<String>, Option<Progress>, Option<crate::ask::Ask>, TurnInfo) {
        if kind == "antigravity" {
            return self.update_agy(path);
        }
        let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let fc = self.files.entry(path.to_path_buf()).or_default();
        if len < fc.len {
            *fc = FileCache::default();
        }
        if len > fc.offset {
            if let Ok(mut f) = std::fs::File::open(path) {
                if f.seek(SeekFrom::Start(fc.offset)).is_ok() {
                    let mut buf = Vec::new();
                    let _ = f.take(len - fc.offset).read_to_end(&mut buf);
                    // only consume complete lines
                    let end = buf.iter().rposition(|b| *b == b'\n').map(|i| i + 1).unwrap_or(0);
                    for line in buf[..end].split(|b| *b == b'\n') {
                        if !line.is_empty() {
                            let text = String::from_utf8_lossy(line);
                            // open-ask tracking: only lines that can open or close a question
                            if let Some(h) = crate::harness::get(kind) {
                                h.feed_turn(&text, &mut fc.turn);
                            }
                            let relevant = text.contains("\"ask\"")
                                || text.contains("AskUserQuestion")
                                || (fc.ask.is_some() && (text.contains("toolResult") || text.contains("tool_result") || text.contains("\"user\"")));
                            if relevant {
                                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                                    if let Some(h) = crate::harness::get(kind) {
                                        h.feed_ask(&v, &mut fc.ask);
                                    }
                                }
                            }
                            parse_line(kind, &text, fc);
                        }
                    }
                    fc.offset += end as u64;
                }
            }
        }
        fc.len = len;
        (fc.model.clone(), fc.progress.clone(), fc.ask.clone(), fc.turn.clone())
    }

    /// agy conversation db: new steps since the last seen idx (kept in `offset`).
    fn update_agy(&mut self, db: &Path) -> (Option<String>, Option<Progress>, Option<crate::ask::Ask>, TurnInfo) {
        let fc = self.files.entry(db.to_path_buf()).or_default();
        if let Ok(steps) = crate::agy::read_steps(db, fc.offset as i64) {
            for s in &steps {
                if s.kind == crate::agy::USER && !s.text.as_deref().unwrap_or("").trim_start().starts_with("[adash ") {
                    fc.turn.last_human = Some(fc.turn.last_human.map_or(s.ts, |h| h.max(s.ts)));
                }
                if s.kind == crate::agy::MODEL && s.status == crate::agy::FINISHED {
                    fc.turn.last_answer = Some(fc.turn.last_answer.map_or(s.ts, |h| h.max(s.ts)));
                }
                fc.seen = fc.seen.max(s.idx as u64);
            }
            // steps still running are read again next time (agy updates them in place)
            fc.offset = crate::agy::resume_after(fc.offset as i64, &steps).max(0) as u64;
            if let Some(p) = crate::agy::phase(&steps) {
                // keep the phase start while it doesn't change
                if fc.turn.phase.is_none_or(|(o, _)| o != p.0) {
                    fc.turn.phase = Some(p);
                }
            }
        }
        (fc.model.clone(), fc.progress.clone(), None, fc.turn.clone())
    }

    pub fn retain(&mut self, live: &HashSet<PathBuf>) {
        self.files.retain(|p, _| live.contains(p));
    }
}

/// omp rewrites the padded title record in place on line 1.
pub fn omp_title(path: &Path) -> Option<String> {
    let f = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(f).take(64 * 1024).read_line(&mut line).ok()?;
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    (v["type"] == "title").then(|| v["title"].as_str().map(|s| s.trim().to_string())).flatten()
}

/// Directories where a new session file of this harness for `cwd` will appear.
pub fn new_session_dirs(kind: &str, cwd: &Path) -> Vec<PathBuf> {
    match kind {
        "omp" => vec![omp_slug_dir(cwd)],
        "claude" => {
            let slug: String = cwd.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
            vec![home().join(".claude/projects").join(slug)]
        }
        "codex" => vec![home().join(".codex/sessions").join(chrono::Local::now().format("%Y/%m/%d").to_string())],
        "antigravity" => vec![crate::agy::root().join("conversations")],
        _ => vec![],
    }
}

/// (entries, newest mtime) of the directories; changes when a session file is created.
pub fn dirs_signature(dirs: &[PathBuf]) -> (usize, i64) {
    let mut n = 0;
    let mut m = 0;
    for d in dirs {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                n += 1;
                m = m.max(mtime_ms(&e.path()));
            }
        }
        m = m.max(mtime_ms(d));
    }
    (n, m)
}

pub fn omp_slug_dir(cwd: &Path) -> PathBuf {
    let base = home().join(".omp/agent/sessions");
    let h = home();
    let rel = cwd.strip_prefix(&h).map(|p| p.to_path_buf()).unwrap_or_else(|_| cwd.to_path_buf());
    let slug = format!("-{}", rel.to_string_lossy().trim_start_matches('/').replace('/', "-"));
    base.join(slug)
}

fn jsonl_in(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "jsonl")).collect()).unwrap_or_default()
}

/// Strip spinner / glyph / app prefixes: "π ⠋ 󰈙 EN: text" → "EN: text".
pub fn clean_title(t: &str) -> String {
    let mut s = t.trim();
    loop {
        let before = s;
        for pre in ["OMP -", "OMP", "π -", "π"] {
            if let Some(r) = s.strip_prefix(pre) {
                if r.is_empty() || r.starts_with(' ') {
                    s = r.trim_start();
                }
            }
        }
        if let Some(c) = s.chars().next() {
            let spinner = ('\u{2800}'..='\u{28FF}').contains(&c);
            let private = ('\u{E000}'..='\u{F8FF}').contains(&c) || c as u32 >= 0xF0000;
            if (spinner || private || c == '-' || c == '·') && s[c.len_utf8()..].starts_with(' ') || (spinner || private) {
                s = s[c.len_utf8()..].trim_start();
            }
        }
        if s == before {
            return s.to_string();
        }
    }
}

/// Resolve the session file for each agent process (keyed by handle).
pub fn resolve_sessions(
    procs: &HashMap<String, Vec<Proc>>,
    titles: &HashMap<String, String>, // handle → cleaned terminal title
    cache: &mut SessionCache,
) -> HashMap<String, SessionInfo> {
    if !cache.pins_loaded {
        cache.sticky = load_pins();
        cache.pins_loaded = true;
    }
    let mut out: HashMap<String, SessionInfo> = HashMap::new();
    let mut claimed: HashSet<PathBuf> = HashSet::new();
    let mut pending_omp: Vec<&Proc> = Vec::new();

    let main_of = |v: &Vec<Proc>| v.first().cloned();
    // pass 1: exact omp mappings — the session file the process holds open (fd), else --resume.
    // omp keeps its current session jsonl open; `--resume` is only what it started with.
    for (h, v) in procs {
        let Some(p) = main_of(v) else { continue };
        if p.kind != "omp" {
            continue;
        }
        // a subagent file the process has open stands for its main session
        let fd = fd_match(p.pid, |l| l.contains("/.omp/agent/sessions/") && l.ends_with(".jsonl")).and_then(|f| omp_main(&f)).filter(|f| !claimed.contains(f));
        let resume = arg_value(&p.args, "--resume").map(PathBuf::from).and_then(|r| omp_main(&r)).filter(|r| r.exists());
        if let Some(f) = fd {
            claimed.insert(f.clone());
            out.insert(h.clone(), SessionInfo { path: Some(f), exact: true, confidence: "exact", ..Default::default() });
        } else if let Some(r) = resume {
            claimed.insert(r.clone());
            out.insert(h.clone(), SessionInfo { path: Some(r), exact: true, confidence: "exact", ..Default::default() });
        } else {
            pending_omp.push(v.first().unwrap());
        }
    }
    // pass 2: title match — terminal title "CODE: title" equals the session file's latest title
    // record. Exactly one file → "title"; several → newest modified since process start.
    let mut dir_files: HashMap<PathBuf, Vec<(PathBuf, i64, i64, Option<String>)>> = HashMap::new();
    for p in &pending_omp {
        let dir = omp_slug_dir(&p.cwd);
        dir_files.entry(dir.clone()).or_insert_with(|| {
            jsonl_in(&dir)
                .into_iter()
                .map(|f| {
                    let name = f.file_name().unwrap().to_string_lossy().into_owned();
                    let created = omp_file_created_ms(&name).unwrap_or(0);
                    let m = mtime_ms(&f);
                    let t = omp_title(&f).map(|t| clean_title(&t));
                    (f, created, m, t)
                })
                .collect()
        });
    }
    let mut rest: Vec<&Proc> = Vec::new();
    for p in pending_omp {
        let files = &dir_files[&omp_slug_dir(&p.cwd)];
        let tt = titles.get(&p.handle).cloned().unwrap_or_default();
        let same: Vec<&(PathBuf, i64, i64, Option<String>)> = if tt.is_empty() {
            vec![]
        } else {
            files.iter().filter(|(f, _, _, t)| !claimed.contains(f) && t.as_deref().is_some_and(|t| !t.is_empty() && titles_equal(&tt, t))).collect()
        };
        let hit = match same.len() {
            0 => None,
            1 => Some(same[0]),
            _ => same.iter().filter(|x| x.2 >= p.start_ms).max_by_key(|x| x.2).copied(),
        };
        // the same process (pid + start time) keeps the file it was mapped to: omp closes its
        // session file between writes and the terminal title changes ("action required")
        let pinned = cache
            .sticky
            .get(&p.handle)
            .filter(|s| s.0 == p.pid && (s.1 - p.start_ms).abs() < 2000 && s.3 != "heuristic" && s.2.exists() && !claimed.contains(&s.2))
            .map(|s| (s.2.clone(), s.3));
        if let Some((f, conf)) = pinned {
            claimed.insert(f.clone());
            out.insert(p.handle.clone(), SessionInfo { path: Some(f), exact: true, confidence: conf, ..Default::default() });
        } else if let Some((f, ..)) = hit {
            claimed.insert(f.clone());
            out.insert(p.handle.clone(), SessionInfo { path: Some(f.clone()), exact: true, confidence: "title", ..Default::default() });
        } else if let Some((f, conf)) =
            cache.sticky.get(&p.handle).filter(|s| s.0 == p.pid && s.2.exists() && !claimed.contains(&s.2)).map(|s| (s.2.clone(), s.3))
        {
            // keep the previous guess for this process instead of flapping
            claimed.insert(f.clone());
            out.insert(p.handle.clone(), SessionInfo { path: Some(f), exact: conf != "heuristic", confidence: conf, ..Default::default() });
        } else {
            rest.push(p);
        }
    }
    // pass 3: the newest file created after the process started (a guess: "heuristic")
    let mut pairs: Vec<(i64, &Proc, &PathBuf)> = Vec::new();
    for p in &rest {
        let files = &dir_files[&omp_slug_dir(&p.cwd)];
        for (f, c, ..) in files.iter().filter(|(f, c, ..)| !claimed.contains(f) && *c >= p.start_ms - 5000) {
            pairs.push((*c, p, f));
        }
    }
    pairs.sort_by_key(|x| std::cmp::Reverse(x.0));
    for (_, p, f) in pairs {
        if out.contains_key(&p.handle) || claimed.contains(f) {
            continue;
        }
        claimed.insert(f.clone());
        out.insert(p.handle.clone(), SessionInfo { path: Some(f.clone()), exact: false, ..Default::default() });
    }
    // claude / codex
    for (h, v) in procs {
        let Some(p) = main_of(v) else { continue };
        let found = crate::harness::get(&p.kind).and_then(|h| h.session_of(&p));
        if let Some((f, exact)) = found {
            out.insert(h.clone(), SessionInfo { path: Some(f), exact, ..Default::default() });
        }
    }
    // parse
    let mut live = HashSet::new();
    for (h, info) in out.iter_mut() {
        let kind = procs[h][0].kind.clone();
        let Some(path) = info.path.clone() else { continue };
        live.insert(path.clone());
        let (model, mut progress, ask, turn) = cache.update(&kind, &path);
        info.ask = ask;
        info.turn = turn;
        if kind == "omp" {
            info.title = omp_title(&path).map(|t| clean_title(&t)).filter(|t| !t.is_empty());
        }
        if info.confidence.is_empty() {
            info.confidence = if info.exact { "exact" } else { "heuristic" };
        }
        if !info.exact {
            if let Some(pr) = progress.as_mut() {
                pr.source = "heuristic".into();
            }
        }
        info.model = model;
        info.progress = progress;
    }
    cache.retain(&live);
    let next: HashMap<String, (u32, i64, PathBuf, &'static str)> = out
        .iter()
        .filter(|(h, _)| procs[*h][0].kind == "omp")
        .filter_map(|(h, i)| Some((h.clone(), (procs[h][0].pid, procs[h][0].start_ms, i.path.clone()?, i.confidence))))
        .collect();
    if next != cache.sticky {
        let j: serde_json::Map<String, Value> =
            next.iter().map(|(h, (pid, st, p, c))| (h.clone(), serde_json::json!({"pid": pid, "start": st, "path": p, "conf": c}))).collect();
        let _ = crate::util::atomic_write(&pins_path(), Value::Object(j).to_string().as_bytes());
    }
    cache.sticky = next;
    out
}

fn fd_match(pid: u32, pred: impl Fn(&str) -> bool) -> Option<PathBuf> {
    crate::platform::open_files(pid).into_iter().find(|l| pred(&l.to_string_lossy()))
}

pub(crate) fn claude_file(p: &Proc) -> Option<(PathBuf, bool)> {
    if let Some(f) = fd_match(p.pid, |l| l.contains("/.claude/projects/") && l.ends_with(".jsonl")) {
        return Some((f, true));
    }
    let slug: String = p.cwd.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let dir = home().join(".claude/projects").join(slug);
    jsonl_in(&dir).into_iter().map(|f| (mtime_ms(&f), f)).filter(|(m, _)| *m >= p.start_ms).max_by_key(|x| x.0).map(|x| (x.1, false))
}

pub(crate) fn codex_file(p: &Proc) -> Option<(PathBuf, bool)> {
    if let Some(f) = fd_match(p.pid, |l| l.contains("/.codex/sessions/") && l.ends_with(".jsonl")) {
        return Some((f, true));
    }
    let root = home().join(".codex/sessions");
    let mut best: Option<(i64, PathBuf)> = None;
    for back in 0..3 {
        let d = chrono::Local::now() - chrono::Duration::days(back);
        let dir = root.join(d.format("%Y/%m/%d").to_string());
        for f in jsonl_in(&dir) {
            let m = mtime_ms(&f);
            if m < p.start_ms || best.as_ref().is_some_and(|b| b.0 >= m) {
                continue;
            }
            let Ok(file) = std::fs::File::open(&f) else { continue };
            let mut first = String::new();
            let _ = BufReader::new(file).take(256 * 1024).read_line(&mut first);
            let cwd_ok = serde_json::from_str::<Value>(&first).ok().and_then(|v| v["payload"]["cwd"].as_str().map(|c| Path::new(c) == p.cwd)).unwrap_or(false);
            if cwd_ok {
                best = Some((m, f));
            }
        }
    }
    best.map(|b| (b.1, false))
}

#[cfg(test)]
mod tests {
    /// agy writes a model step while it runs and updates the same row to FINISHED later; the
    /// agent must turn from working to done when that update lands (2026-11-04: Argon agents
    /// stuck "working" after finishing).
    #[test]
    fn agy_step_finished_in_place_ends_the_turn() {
        let dir = std::env::temp_dir().join(format!("adash-agy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("c.db");
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch(
            "CREATE TABLE steps (idx INTEGER PRIMARY KEY, step_type INTEGER, status INTEGER, metadata BLOB, step_payload BLOB);
             INSERT INTO steps VALUES (1, 14, 3, NULL, NULL), (2, 15, 1, NULL, NULL);",
        )
        .unwrap();
        let mut cache = super::SessionCache::default();
        let t = cache.update_agy(&db).3;
        assert_eq!(t.phase.map(|p| p.0), Some(true), "model step running: working");
        assert!(t.last_answer.is_none());
        let t = cache.update_agy(&db).3;
        assert_eq!(t.phase.map(|p| p.0), Some(true), "nothing changed yet");
        c.execute("UPDATE steps SET status = 3 WHERE idx = 2", []).unwrap();
        let t = cache.update_agy(&db).3;
        assert_eq!(t.phase.map(|p| p.0), Some(false), "the same row finished: done");
        assert!(t.last_answer.is_some());
        // a new turn after it is read normally
        c.execute_batch("INSERT INTO steps VALUES (3, 14, 3, NULL, NULL), (4, 15, 1, NULL, NULL);").unwrap();
        assert_eq!(cache.update_agy(&db).3.phase.map(|p| p.0), Some(true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::titles_equal;

    #[test]
    fn turn_state_follows_the_session_record() {
        use super::{feed_turn, TurnInfo};
        let mut t = TurnInfo::default();
        feed_turn(
            "omp",
            r#"{"type":"message","timestamp":"2026-10-29T00:00:01.000Z","message":{"role":"user","content":[{"type":"text","text":"해 주세요"}]}}"#,
            &mut t,
        );
        assert_eq!((t.phase, t.last_human), (Some((true, 1793232001000)), Some(1793232001000)));
        feed_turn("omp", r#"{"type":"message","timestamp":"2026-10-29T00:00:05.000Z","message":{"role":"assistant","stopReason":"toolUse"}}"#, &mut t);
        assert_eq!(t.phase, Some((true, 1793232001000)), "still working; keeps the phase start");
        feed_turn("omp", r#"{"type":"message","timestamp":"2026-10-29T00:00:09.000Z","message":{"role":"assistant","stopReason":"stop"}}"#, &mut t);
        assert_eq!((t.phase, t.last_answer), (Some((false, 1793232009000)), Some(1793232009000)));
        // an adash relay pointer opens a turn but is not a human message
        feed_turn(
            "omp",
            r#"{"type":"message","timestamp":"2026-10-29T00:00:20.000Z","message":{"role":"user","content":[{"type":"text","text":"[adash T1 reply from X] 답변: /x/reply.md"}]}}"#,
            &mut t,
        );
        assert_eq!((t.phase, t.last_human), (Some((true, 1793232020000)), Some(1793232001000)));
        let mut c = TurnInfo::default();
        feed_turn("codex", r#"{"timestamp":"2026-10-29T00:00:01.000Z","type":"event_msg","payload":{"type":"task_started"}}"#, &mut c);
        feed_turn("codex", r#"{"timestamp":"2026-10-29T00:00:01.500Z","type":"event_msg","payload":{"type":"user_message","message":"hi"}}"#, &mut c);
        feed_turn("codex", r#"{"timestamp":"2026-10-29T00:00:02.000Z","type":"event_msg","payload":{"type":"task_complete"}}"#, &mut c);
        assert_eq!((c.phase, c.last_human, c.last_answer), (Some((false, 1793232002000)), Some(1793232001500), Some(1793232002000)));
    }

    #[test]
    fn subagent_files_map_to_their_main_session() {
        use super::*;
        let root = std::env::temp_dir().join(format!("adash-omp-sessions-{}", std::process::id()));
        let slug = root.join("-work-galaxy-survey");
        let stem = "2026-10-31T05-54-49-376Z_01a19086-65a0-77f0-8db9-e3fd1bea686f";
        std::fs::create_dir_all(slug.join(stem).join("nested")).unwrap();
        let main = slug.join(format!("{stem}.jsonl"));
        std::fs::write(&main, "").unwrap();
        assert_eq!(omp_main_in(&root, &main), Some(main.clone()));
        assert_eq!(omp_main_in(&root, &slug.join(stem).join("RsSubsectionTitles.jsonl")), Some(main.clone()));
        assert_eq!(omp_main_in(&root, &slug.join(stem).join("nested/Deeper.jsonl")), Some(main.clone()));
        assert_eq!(omp_main_in(&root, Path::new("/elsewhere/x.jsonl")), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn provider_errors_stop_the_turn_until_the_next_message() {
        use super::*;
        let mut t = TurnInfo::default();
        feed_turn(
            "omp",
            r#"{"type":"message","timestamp":"2026-10-29T00:00:01.000Z","message":{"role":"user","content":[{"type":"text","text":"go"}]}}"#,
            &mut t,
        );
        feed_turn(
            "omp",
            r#"{"type":"message","timestamp":"2026-10-29T00:00:05.000Z","message":{"role":"assistant","content":[],"stopReason":"error","errorStatus":403,"errorMessage":"403 {\"error\":{\"type\":\"forbidden\",\"message\":\"Request not allowed\"}}","provider":"anthropic","model":"claude-opus-5"}}"#,
            &mut t,
        );
        let e = t.error.clone().unwrap();
        assert_eq!((e.status, e.message.as_str(), e.provider.as_deref()), (Some(403), "Request not allowed", Some("anthropic")));
        assert_eq!(t.phase.map(|p| p.0), Some(false), "the turn is over");
        // an auto-retry that produced output clears it
        feed_turn(
            "omp",
            r#"{"type":"message","timestamp":"2026-10-29T00:00:09.000Z","message":{"role":"assistant","content":[{"type":"text","text":"ok"}],"stopReason":"stop"}}"#,
            &mut t,
        );
        assert!(t.error.is_none());
        // claude: status from the text; codex: unauthorized → 401
        let c: Value = serde_json::from_str(r#"{"type":"assistant","isApiErrorMessage":true,"message":{"model":"<synthetic>","content":[{"type":"text","text":"API Error: 529 {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}"}]}}"#).unwrap();
        let e = record_error("claude", &c, 1).unwrap();
        assert_eq!((e.status, e.message.as_str(), e.model), (Some(529), "Overloaded", None));
        let x: Value = serde_json::from_str(
            r#"{"type":"event_msg","payload":{"type":"error","message":"Your access token could not be refreshed.","codex_error_info":"unauthorized"}}"#,
        )
        .unwrap();
        assert_eq!(record_error("codex", &x, 1).unwrap().status, Some(401));
        let mut t = TurnInfo::default();
        feed_turn("codex", r#"{"timestamp":"2026-10-29T00:00:01.000Z","type":"event_msg","payload":{"type":"error","message":"stream disconnected"}}"#, &mut t);
        assert!(t.error.is_some());
        feed_turn("codex", r#"{"timestamp":"2026-10-29T00:00:02.000Z","type":"event_msg","payload":{"type":"user_message","message":"again"}}"#, &mut t);
        assert!(t.error.is_none(), "the next user message clears it");
    }

    #[test]
    fn title_match_is_exact_or_terminal_truncated() {
        assert!(titles_equal("PLANNER: Draft survey proposal", "PLANNER: Draft survey proposal"));
        assert!(titles_equal("DASH: Prioritize and Organize Job Ap…", "DASH: Prioritize and Organize Job Applications"));
        assert!(!titles_equal("PLANNER: Draft survey proposal", "PLANNER: Draft survey proposal v2"));
        assert!(!titles_equal("…", "anything"));
    }
}
