//! State history for the timeline view: recorded transitions (`~/.local/state/adash/history.jsonl`)
//! plus past segments reconstructed from session-file turn timestamps.
use crate::collect::{Agent, Snapshot};
use crate::meta::{Meta, ProjectMeta};
use crate::threads::Thread;
use crate::util::{atomic_write, state_dir};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};

const KEEP_MS: i64 = 14 * 86_400_000;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Rec {
    pub ts: i64,
    pub identity: String,
    pub name: String,
    pub project: String,
    pub state: String,
    pub prev_state: Option<String>,
}

fn path() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join(format!("adash-test-history-{}.jsonl", std::process::id()));
    }
    state_dir().join("history.jsonl")
}

/// working | waiting (needs human) | snoozed | idle
pub fn norm_state(a: &Agent) -> &'static str {
    if a.state == "asking" && a.snoozed_until.is_none() {
        "asking"
    } else if a.state == "working" {
        "working"
    } else if a.snoozed_until.is_some() && a.state != "idle" {
        "snoozed"
    } else if a.needs_human {
        "waiting"
    } else {
        "idle"
    }
}

/// Incremental turn parser for one session file.
#[derive(Default)]
struct Recon {
    /// agy: step idx the next read starts after
    offset: u64,
    pos: crate::util::JsonlPos,
    /// agy: highest step idx already fed (steps before it re-read only for their status)
    seen: u64,
    /// (ts, state) transitions, ascending
    trans: Vec<(i64, &'static str)>,
    turn_open: bool,
    last_activity: i64,
    /// user messages: (ts, first 120 chars whitespace collapsed, extra interaction fields:
    /// `skill` for a skill invocation, `askAnswer` for an ask answer)
    users: Vec<(i64, String, Value)>,
}

impl Recon {
    fn user(&mut self, ts: i64, text: &str) {
        self.user_with(ts, text, Value::Null);
    }
    fn user_with(&mut self, ts: i64, text: &str, meta: Value) {
        // a message typed mid-turn (steering) is not a wait; require a real gap
        if self.turn_open && self.last_activity > 0 && ts - self.last_activity > 2_000 {
            self.trans.push((self.last_activity, "waiting"));
        }
        if self.trans.last().map(|t| t.1) != Some("working") {
            self.trans.push((ts, "working"));
        }
        self.turn_open = true;
        self.last_activity = ts;
        let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        self.users.push((ts, one.chars().take(120).collect(), meta));
    }
    fn activity(&mut self, ts: i64) {
        if self.turn_open {
            self.last_activity = self.last_activity.max(ts);
        }
    }
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter(|c| c["type"] == "text").filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

fn ts_of(v: &Value) -> Option<i64> {
    v.as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.timestamp_millis())
}

fn feed(kind: &str, r: &Value, rc: &mut Recon) {
    match kind {
        "omp" => {
            let Some(ts) = ts_of(&r["timestamp"]) else { return };
            if let Some((text, skill)) = crate::transcript::omp_user(r) {
                rc.user_with(ts, &text, skill.map(|s| json!({"skill": {"name": s["name"], "path": s["path"]}})).unwrap_or(Value::Null));
                return;
            }
            if r["type"] != "message" {
                return;
            }
            if r["message"]["role"] == "toolResult" && r["message"]["toolName"] == "ask" {
                let items = crate::ask::answer_items(&r["message"]["details"]);
                if !items.is_empty() {
                    rc.user_with(ts, &crate::ask::answer_text(&items), json!({"askAnswer": items}));
                    return;
                }
            }
            match r["message"]["role"].as_str() {
                Some("assistant") | Some("toolResult") => rc.activity(ts),
                _ => {}
            }
        }
        "claude" => {
            let Some(ts) = ts_of(&r["timestamp"]) else { return };
            match r["type"].as_str() {
                Some("user") => {
                    let content = &r["message"]["content"];
                    let tool_result = content.as_array().is_some_and(|a| a.iter().any(|c| c["type"] == "tool_result"));
                    let answered = content
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|c| c["type"] == "tool_result")
                        .map(|c| crate::ask::answer_items(&c["content"]))
                        .find(|i| !i.is_empty());
                    if let Some(items) = answered {
                        rc.user_with(ts, &crate::ask::answer_text(&items), json!({"askAnswer": items}));
                    } else if tool_result || r["isMeta"] == true {
                        rc.activity(ts);
                    } else {
                        let raw = text_of(content);
                        match crate::transcript::claude_command(&raw) {
                            Some((typed, sk)) => rc.user_with(ts, &typed, json!({"skill": {"name": sk["name"], "path": Value::Null}})),
                            None => rc.user(ts, &raw),
                        }
                    }
                }
                Some("assistant") => rc.activity(ts),
                _ => {}
            }
        }
        "codex" => {
            if r["type"] != "response_item" {
                return;
            }
            let Some(ts) = ts_of(&r["timestamp"]) else { return };
            let p = &r["payload"];
            let text = p["content"].as_array().map(|a| a.iter().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default();
            let user_text = p["type"] == "message" && p["role"] == "user" && !text.trim().is_empty() && !text.trim_start().starts_with('<');
            if user_text {
                rc.user(ts, &text);
            } else {
                rc.activity(ts);
            }
        }
        _ => {}
    }
}

impl Recon {
    fn update(&mut self, kind: &str, p: &Path) {
        if kind == "antigravity" {
            // conversation db: step idx in `offset`
            if let Ok(steps) = crate::agy::read_steps(p, self.offset as i64) {
                self.offset = crate::agy::resume_after(self.offset as i64, &steps).max(0) as u64;
                for s in steps {
                    // a step re-read because it was still running: only its activity is new
                    // `seen` = one past the highest idx fed (step 0 is a real step)
                    let again = (s.idx as u64) < self.seen;
                    self.seen = self.seen.max(s.idx as u64 + 1);
                    if s.ts <= 0 || (again && s.kind == crate::agy::USER) {
                        continue;
                    }
                    if s.kind == crate::agy::USER {
                        self.user(s.ts, s.text.as_deref().unwrap_or(""));
                    } else {
                        self.activity(s.ts);
                    }
                }
            }
            return;
        }
        if self.pos.moved(p) {
            *self = Recon::default();
        }
        let mut pos = std::mem::take(&mut self.pos);
        pos.read(p, |_, line| {
            if let Ok(r) = serde_json::from_slice::<Value>(line) {
                feed(kind, &r, self);
            }
        });
        self.pos = pos;
    }

    /// Transitions including the close of the last turn (if the agent is not working now).
    fn transitions(&self, working_now: bool) -> Vec<(i64, &'static str)> {
        let mut t = self.trans.clone();
        if self.turn_open && !working_now && self.last_activity > 0 && t.last().is_some_and(|x| x.0 < self.last_activity) {
            t.push((self.last_activity, "waiting"));
        }
        t
    }
}

#[derive(Default)]
pub struct History {
    recs: Vec<Rec>,
    last: HashMap<String, Rec>,
    recon: HashMap<PathBuf, Recon>,
    /// dashboard chat sends by the human: (ts, identity, first 120 chars collapsed)
    sends: Vec<(i64, String, String)>,
    agent_sends: Vec<(i64, Option<String>, String, String, Option<String>)>,
    /// identities of throwaway sessions (`test: true` launches): never recorded or shown
    test_ids: std::collections::HashSet<String>,
}

/// Terminal handles of `test: true` launches.
pub fn test_handles() -> std::collections::HashSet<String> {
    let p = if cfg!(test) {
        std::env::temp_dir().join(format!("adash-test-handles-{}.txt", std::process::id()))
    } else {
        crate::analytics::data_dir().join("test_handles.txt")
    };
    std::fs::read_to_string(p).unwrap_or_default().lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
}

fn test_ids_path() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join(format!("adash-test-identities-{}.txt", std::process::id()));
    }
    crate::analytics::data_dir().join("test_identities.txt")
}

/// Identity renames for the analytics store (state_transitions), drained by the ingest loop.
pub static REKEYS: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

/// Harness of a session file by its location.
pub fn harness_of(p: &Path) -> Option<&'static str> {
    let s = p.to_string_lossy();
    if s.contains("/.omp/agent/sessions/") {
        Some("omp")
    } else if s.contains("/.claude/projects/") {
        Some("claude")
    } else if s.contains("/.codex/sessions/") {
        Some("codex")
    } else {
        None
    }
}

fn collapse120(t: &str) -> String {
    t.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(120).collect()
}

/// Append a `{"kind":"send"}` line for a message the human sent from the dashboard chat.
pub fn append_send(ts: i64, identity: &str, text: &str, channel: &str) {
    if let Some(dir) = path().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path()) {
        let line = json!({"ts": ts, "identity": identity, "kind": "send", "text": collapse120(text), "channel": channel});
        let _ = f.write_all(format!("{line}\n").as_bytes());
    }
}

impl History {
    /// Load, drop entries older than 14 days and rewrite the file compacted.
    pub fn load(now: i64) -> History {
        let text = std::fs::read_to_string(path()).unwrap_or_default();
        let lines: Vec<(Value, &str)> = text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok().map(|v| (v, l))).collect();
        let kept: Vec<&(Value, &str)> = lines.iter().filter(|(v, _)| v["ts"].as_i64().is_some_and(|t| now - t < KEEP_MS)).collect();
        if kept.len() != lines.len() {
            let body: String = kept.iter().map(|(_, l)| format!("{l}\n")).collect();
            let _ = atomic_write(&path(), body.as_bytes());
        }
        let mut recs = Vec::new();
        let mut sends = Vec::new();
        for (v, _) in kept {
            if v["kind"] == "send" {
                sends.push((v["ts"].as_i64().unwrap_or(0), v["identity"].as_str().unwrap_or("").to_string(), v["text"].as_str().unwrap_or("").to_string()));
            } else if let Ok(r) = serde_json::from_value::<Rec>(v.clone()) {
                recs.push(r);
            }
        }
        let mut last = HashMap::new();
        for r in &recs {
            last.insert(r.identity.clone(), r.clone());
        }
        let test_ids = std::fs::read_to_string(test_ids_path()).unwrap_or_default().lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        let mut h = History { recs, last, recon: HashMap::new(), sends, agent_sends: Vec::new(), test_ids };
        h.purge_tests(&[]);
        h
    }

    /// Mark identities as test sessions and drop their records (history file rewritten when
    /// anything goes). Identities that embed a test handle (pane keys) count too.
    pub fn purge_tests(&mut self, more: &[String]) {
        let fresh: Vec<&String> = more.iter().filter(|i| !self.test_ids.contains(*i)).collect();
        if !fresh.is_empty() {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(test_ids_path()) {
                let _ = f.write_all(fresh.iter().map(|i| format!("{i}\n")).collect::<String>().as_bytes());
            }
            self.test_ids.extend(fresh.into_iter().cloned());
        }
        let handles = test_handles();
        let is_test = |id: &str| self.test_ids.contains(id) || handles.iter().any(|h| id.contains(h.as_str()));
        let before = self.recs.len();
        let recs: Vec<Rec> = self.recs.drain(..).filter(|r| !is_test(&r.identity)).collect();
        self.recs = recs;
        if self.recs.len() == before {
            return;
        }
        self.last.retain(|id, _| !(self.test_ids.contains(id) || handles.iter().any(|h| id.contains(h.as_str()))));
        if cfg!(test) {
            return; // unit tests share one history file
        }
        // rewrite the file without them (sends kept)
        let text = std::fs::read_to_string(path()).unwrap_or_default();
        let body: String = text
            .lines()
            .filter(|l| {
                serde_json::from_str::<Value>(l).ok().is_none_or(|v| {
                    v["kind"] == "send" || !v["identity"].as_str().is_some_and(|i| self.test_ids.contains(i) || handles.iter().any(|h| i.contains(h.as_str())))
                })
            })
            .map(|l| format!("{l}\n"))
            .collect();
        let _ = atomic_write(&path(), body.as_bytes());
    }

    /// Projects with recorded agents (closed and offline ones included).
    pub fn has_project(&self, name: &str) -> bool {
        self.recs.iter().any(|r| r.project == name)
    }

    /// Agent→agent sends (sender, full text or the sender's shell command), refreshed from the
    /// analytics store after each ingest and appended live on `adash tell` from an agent.
    pub fn set_agent_sends(&mut self, v: Vec<(i64, Option<String>, String, String, Option<String>)>) {
        self.agent_sends = v;
    }

    pub fn record_agent_send(&mut self, ts: i64, from: &str, text: &str, channel: &str, to: &str) {
        self.agent_sends.push((ts, Some(from.to_string()), text.to_string(), channel.to_string(), Some(to.to_string())));
    }

    pub fn record_send(&mut self, ts: i64, identity: &str, text: &str, channel: &str) {
        append_send(ts, identity, text, channel);
        self.sends.push((ts, identity.to_string(), collapse120(text)));
    }

    /// One session user message: `human` | `agent` | `relay` | `system`, with the sender when
    /// known. Dashboard chat is human; an agent's `adash tell` / `adash term send`, recorded or
    /// found in the sender's shell command, is
    /// agent; review-script pointer lines are agent
    /// (requester from the registered thread); adash's own lines are relay / system.
    fn classify(
        &self,
        identity: &str,
        who: (&str, Option<&str>),
        ts: i64,
        text: &str,
        markers: &HashMap<String, String>,
    ) -> (&'static str, Option<String>, &'static str) {
        let key: String = text.chars().take(40).collect();
        // an agent's send to THIS agent (recorded target, or the sender's command names it);
        // checked first so an agent typing "[adash] 사용자 메시지…" is not taken for the human
        if text.chars().count() >= 12 {
            let to_me = |to: &Option<String>, cmd: &str| match to {
                Some(t) => t == who.0 || who.1 == Some(t.as_str()),
                None => cmd.contains(who.0) || who.1.is_some_and(|h| cmd.contains(h)),
            };
            if let Some((_, from, _, ch, _)) =
                self.agent_sends.iter().find(|(t, _, s, _, to)| ts - t >= -60_000 && ts - t <= 600_000 && s.contains(key.as_str()) && to_me(to, s))
            {
                return (
                    "agent",
                    from.clone(),
                    match ch.as_str() {
                        "term-send" => "term-send",
                        _ => "cli-tell",
                    },
                );
            }
        }
        if text.starts_with("[adash] 사용자 메시지") {
            return ("human", None, "dashboard"); // dashboard chat, multi-line file mode
        }
        if self.sends.iter().any(|(t, id, s)| id == identity && (ts - t).abs() <= 300_000 && s.chars().take(40).collect::<String>() == key) {
            return ("human", None, "dashboard");
        }
        if crate::analytics::is_peer_pointer(text) {
            let marker = text.strip_prefix('[').and_then(|r| r.split_once(']')).map(|x| x.0);
            return ("agent", marker.and_then(|m| markers.get(m)).cloned(), "peer-review");
        }
        if text.starts_with("[adash] ") {
            ("system", None, "adash")
        } else if text.starts_with("[adash ") {
            ("relay", crate::analytics::relay_from(text), "adash-relay")
        } else {
            ("human", None, "terminal")
        }
    }

    /// Records kept under `from` (a pane key) belong to `to` (the session file): rename in
    /// memory and in history.jsonl. Returns whether anything moved.
    pub fn rekey(&mut self, from: &str, to: &str) -> bool {
        if from == to || !(self.last.contains_key(from) || self.recs.iter().any(|r| r.identity == from) || self.sends.iter().any(|s| s.1 == from)) {
            return false;
        }
        for r in self.recs.iter_mut().filter(|r| r.identity == from) {
            r.identity = to.to_string();
        }
        if let Some(mut r) = self.last.remove(from) {
            r.identity = to.to_string();
            // keep the newer of the two "last" records
            if self.last.get(to).is_none_or(|x| x.ts < r.ts) {
                self.last.insert(to.to_string(), r);
            }
        }
        for s in self.sends.iter_mut().filter(|s| s.1 == from) {
            s.1 = to.to_string();
        }
        self.recs.sort_by_key(|r| r.ts);
        REKEYS.lock().unwrap_or_else(|e| e.into_inner()).push((from.to_string(), to.to_string()));
        if let Ok(text) = std::fs::read_to_string(path()) {
            let out: String = text
                .lines()
                .map(|l| match serde_json::from_str::<Value>(l) {
                    Ok(mut v) if v["identity"] == from => {
                        v["identity"] = json!(to);
                        v.to_string()
                    }
                    _ => l.to_string(),
                })
                .map(|l| l + "\n")
                .collect();
            let _ = crate::util::atomic_write(&path(), out.as_bytes());
        }
        true
    }

    /// Records keyed by an omp subagent file (a mapping bug) move to the main session.
    pub fn rekey_subagents(&mut self) -> usize {
        let ids: std::collections::BTreeSet<String> = self.recs.iter().map(|r| r.identity.clone()).collect();
        let mut n = 0;
        for id in ids {
            if let Some(main) = crate::session::omp_main(Path::new(&id)).filter(|m| m.to_string_lossy() != id) {
                n += self.rekey(&id, &main.to_string_lossy()) as usize;
            }
        }
        n
    }

    /// Append every state change (and agents that disappeared → offline).
    pub fn observe(&mut self, snap: &Snapshot) {
        // an agent first recorded by pane now has its session file: carry its history over
        for a in &snap.agents {
            if a.identity != a.pane_key {
                self.rekey(&a.pane_key, &a.identity);
            }
        }
        // throwaway sessions leave no trace
        let handles = test_handles();
        let tests: Vec<String> = snap
            .agents
            .iter()
            .filter(|a| a.handle.as_ref().is_some_and(|h| handles.contains(h)))
            .flat_map(|a| [a.identity.clone(), a.pane_key.clone()])
            .filter(|i| !self.test_ids.contains(i))
            .collect();
        if !tests.is_empty() {
            self.purge_tests(&tests);
        }
        let mut new = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for a in snap.agents.iter().filter(|a| !self.test_ids.contains(&a.identity)) {
            seen.insert(a.identity.clone());
            let st = norm_state(a);
            let prev = self.last.get(&a.identity).map(|r| r.state.clone());
            if prev.as_deref() != Some(st) {
                new.push(Rec {
                    ts: snap.at,
                    identity: a.identity.clone(),
                    name: a.name.clone(),
                    project: a.project.clone(),
                    state: st.into(),
                    prev_state: prev,
                });
            }
        }
        for (id, r) in &self.last {
            if !seen.contains(id) && r.state != "offline" {
                new.push(Rec {
                    ts: snap.at,
                    identity: id.clone(),
                    name: r.name.clone(),
                    project: r.project.clone(),
                    state: "offline".into(),
                    prev_state: Some(r.state.clone()),
                });
            }
        }
        if new.is_empty() {
            return;
        }
        if let Some(dir) = path().parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path()) {
            let body: String = new.iter().map(|r| serde_json::to_string(r).unwrap() + "\n").collect();
            let _ = f.write_all(body.as_bytes());
        }
        for r in new {
            self.last.insert(r.identity.clone(), r.clone());
            self.recs.push(r);
        }
    }

    pub fn query(&mut self, snap: &Snapshot, meta: &Meta, threads: &[Thread], hours: f64, now: i64) -> Value {
        let from = now - (hours * 3_600_000.0) as i64;
        let markers: HashMap<String, String> =
            threads.iter().filter_map(|t| t.marker.clone().map(|m| (m, t.from.clone()))).filter(|(_, f)| f != "unknown").collect();
        // recorded transitions per identity
        let mut rec: BTreeMap<&str, Vec<(i64, String)>> = BTreeMap::new();
        let mut info: HashMap<&str, (&str, &str)> = HashMap::new();
        for r in &self.recs {
            rec.entry(r.identity.as_str()).or_default().push((r.ts, r.state.clone()));
            info.insert(r.identity.as_str(), (r.name.as_str(), r.project.as_str()));
        }
        let live: HashMap<&str, &Agent> = snap.agents.iter().map(|a| (a.identity.as_str(), a)).collect();
        let mut ids: Vec<&str> = snap.agents.iter().map(|a| a.identity.as_str()).collect();
        for id in rec.keys() {
            if !live.contains_key(id) {
                ids.push(id);
            }
        }

        let mut agents_out = Vec::new();
        let live_names: std::collections::HashSet<&str> = snap.agents.iter().map(|a| a.name.as_str()).collect();
        let mut wait_by_project: HashMap<String, f64> = HashMap::new();
        for id in ids {
            if self.test_ids.contains(id) {
                continue;
            }
            let a = live.get(id).copied();
            let recorded = rec.get(id).cloned().unwrap_or_default();
            let r0 = recorded.first().map(|x| x.0).unwrap_or(i64::MAX);
            let mut trans: Vec<(i64, String, &'static str)> = Vec::new();
            let mut users: Vec<(i64, String, Value)> = Vec::new();
            // live with a trusted mapping, or closed with a session-file identity
            let offline_file = PathBuf::from(id);
            let source: Option<(String, PathBuf, bool)> = match a {
                Some(a) if a.mapping_exact => a.session_path.clone().map(|p| (a.agent_type.clone(), p, a.state == "working")),
                Some(_) => None,
                None if offline_file.is_absolute() && offline_file.exists() => harness_of(&offline_file).map(|k| (k.to_string(), offline_file.clone(), false)),
                None => None,
            };
            if let Some((kind, p, working)) = source {
                {
                    let p = &p;
                    let rc = self.recon.entry(p.clone()).or_default();
                    rc.update(&kind, p);
                    users = rc.users.clone();
                    // omp btw side questions are mine too (btw: true)
                    if kind == "omp" {
                        for e in crate::btw::load(p) {
                            for x in std::iter::once(&e).chain(e.follow_ups.iter()) {
                                users.push((
                                    x.created,
                                    crate::util::truncate(&x.question.split_whitespace().collect::<Vec<_>>().join(" "), 120),
                                    json!({"btw": true}),
                                ));
                            }
                        }
                        users.sort_by_key(|u| u.0);
                    }
                    for (t, s) in rc.transitions(working) {
                        if t < r0 {
                            trans.push((t, s.to_string(), "reconstructed"));
                        }
                    }
                }
            }
            trans.extend(recorded.into_iter().map(|(t, s)| (t, s, "recorded")));
            trans.sort_by_key(|x| x.0);
            // segments clipped to [from, now], merged when adjacent and identical
            let mut segs: Vec<Value> = Vec::new();
            let mut push = |state: &str, start: i64, end: i64, src: &str| {
                let (s, e) = (start.max(from), end.min(now));
                if e <= s {
                    return;
                }
                if let Some(last) = segs.last_mut() {
                    if last["state"] == state && last["src"] == src && last["end"] == s {
                        last["end"] = json!(e);
                        return;
                    }
                }
                segs.push(json!({"state": state, "start": s, "end": e, "src": src}));
            };
            for (i, (t, s, src)) in trans.iter().enumerate() {
                let end = trans.get(i + 1).map(|x| x.0).unwrap_or(now);
                push(s, *t, end, src);
            }
            let (name, project) = match a {
                Some(a) => (a.name.clone(), a.project.clone()),
                None => info.get(id).map(|(n, p)| (n.to_string(), p.to_string())).unwrap_or_default(),
            };
            let waited: f64 =
                segs.iter().filter(|s| s["state"] == "waiting").map(|s| (s["end"].as_i64().unwrap() - s["start"].as_i64().unwrap()) as f64 / 60_000.0).sum();
            *wait_by_project.entry(project.clone()).or_default() += waited;
            // gone agents only if they were live (non-offline) at some point in the window
            if a.is_none() && !segs.iter().any(|s| s["state"] != "offline") {
                continue;
            }
            // who touched this agent: session user messages (exact mapping only)
            let mut interactions = Vec::new();
            let mut last_human: Option<i64> = None;
            for (ts, text, meta) in &users {
                let (kind, sender, channel) = self.classify(id, (name.as_str(), a.and_then(|a| a.handle.as_deref())), *ts, text, &markers);
                if kind == "human" {
                    last_human = Some(last_human.map_or(*ts, |l: i64| l.max(*ts)));
                }
                if *ts >= from && *ts <= now {
                    let mut it = json!({"ts": ts, "kind": kind, "from": sender, "channel": channel, "text": text});
                    if let Value::Object(m) = meta {
                        for (k, v) in m {
                            it[k.as_str()] = v.clone();
                        }
                    }
                    interactions.push(it);
                }
            }
            // a closed agent whose name a live one now uses: "RS (종료됨 10/31)"
            let display = if a.is_none() && live_names.contains(name.as_str()) {
                let at = self.last.get(id).map(|r| r.ts).unwrap_or(now);
                let d = chrono::TimeZone::timestamp_millis_opt(&chrono::Local, at).single().map(|t| t.format("%-m/%-d").to_string()).unwrap_or_default();
                format!("{name} (종료됨 {d})")
            } else {
                name.clone()
            };
            agents_out.push(json!({
                "name": name, "displayName": display, "role": a.and_then(|a| a.role.clone()), "project": project,
                "identity": id, "live": a.is_some(), "segments": segs,
                "interactions": interactions, "lastHumanAt": last_human,
            }));
        }

        let threads_out: Vec<Value> = threads
            .iter()
            .filter(|t| t.created_at >= from || t.events.last().is_some_and(|e| e.at >= from))
            .map(|t| {
                let sends: Vec<i64> = t.events.iter().filter(|e| e.kind == "sent").map(|e| e.at).collect();
                let answered = t.events.iter().find(|e| e.kind == "answered").map(|e| e.at);
                json!({
                    "id": t.id, "from": if t.from == "human" { Value::Null } else { json!(t.from) }, "to": t.to,
                    "createdAt": t.created_at, "deliveredAt": sends.first(),
                    "answeredAt": answered, "relayedAt": if t.status == "relayed" { sends.get(1).copied() } else { None },
                    "status": t.status,
                })
            })
            .collect();

        let default_pm = ProjectMeta::default();
        let mut names: Vec<String> = snap.agents.iter().map(|a| a.project.clone()).chain(wait_by_project.keys().cloned()).collect();
        names.sort();
        names.dedup();
        let projects: Vec<Value> = names
            .iter()
            .filter(|n| !n.is_empty())
            .map(|n| {
                let pm = meta.projects.get(n).unwrap_or(&default_pm);
                json!({
                    "name": n, "score": (pm.score() * 100.0).round() / 100.0,
                    "waitMinWindow": (wait_by_project.get(n).copied().unwrap_or(0.0) * 10.0).round() / 10.0,
                    "waitingNow": snap.agents.iter().filter(|a| &a.project == n && a.needs_human).count(),
                    "workingNow": snap.agents.iter().filter(|a| &a.project == n && a.state == "working").count(),
                })
            })
            .collect();
        json!({"now": now, "from": from, "agents": agents_out, "threads": threads_out, "projects": projects})
    }
}

#[cfg(test)]
mod tests {
    use super::History;

    #[test]
    fn classifies_user_messages() {
        let mut h = History::load(0);
        let mk: std::collections::HashMap<String, String> = [("rv 20261029".to_string(), "CDF".to_string())].into_iter().collect();
        let c = |h: &History, id: &str, ts: i64, t: &str| {
            let (k, f, _) = h.classify(id, ("UI", Some("term_ui")), ts, t, &mk);
            (k, f)
        };
        assert_eq!(c(&h, "id1", 1_000, "그림 다시 그려 주세요"), ("human", None));
        assert_eq!(c(&h, "id1", 1_000, "[adash T20261028-1 from BRIEF] 검토 요청: /x/request.md"), ("relay", Some("BRIEF".into())));
        assert_eq!(c(&h, "id1", 1_000, "[adash T2 reply from UI] 답변: /x/reply.md"), ("relay", Some("UI".into())));
        assert_eq!(c(&h, "id1", 1_000, "[adash] 대시보드 이름 지정: 사용자가 …"), ("system", None));
        assert_eq!(c(&h, "id1", 1_000, "[adash] 사용자 메시지(여러 줄): /inbox/a.md 를 읽고 그 지시를 따르세요."), ("human", None));
        assert_eq!(c(&h, "id1", 1_000, "[rv 20261029] @_guide/b.md 를 읽고 답해줘. 첫 줄에"), ("agent", Some("CDF".into())));
        // the human typed a relay-looking line in the dashboard chat
        h.sends.push((1_000_000, "id1".into(), "[adash T1 from X] looks like relay but was typed in the chat".into()));
        assert_eq!(c(&h, "id1", 1_100_000, "[adash T1 from X] looks like relay but was typed in the chat").0, "human");
        assert_eq!(c(&h, "id2", 1_100_000, "[adash T1 from X] looks like relay but was typed in the chat").0, "relay");
        // a coordinator's `adash tell` (the sender's shell command contains the message start)
        h.set_agent_sends(vec![(
            2_000_000,
            Some("PLANNER".into()),
            "adash tell UI \"UI phase D 계약을 맞춰 주세요: 보고서 목록\"".into(),
            "cli-tell".into(),
            None,
        )]);
        assert_eq!(c(&h, "id3", 2_005_000, "UI phase D 계약을 맞춰 주세요: 보고서 목록"), ("agent", Some("PLANNER".into())));
        assert_eq!(c(&h, "id3", 2_005_000 + 3_600_000, "UI phase D 계약을 맞춰 주세요: 보고서 목록").0, "human"); // too late to be that send
        assert_eq!(c(&h, "id3", 2_005_000, "짧은 말").0, "human");
    }

    #[test]
    fn agent_send_counts_only_for_its_target_and_beats_the_user_prefix() {
        let mut h = super::History::default();
        let mk = std::collections::HashMap::new();
        h.set_agent_sends(vec![(
            1_000_000,
            Some("Lead".into()),
            "adash tell UI \"[adash] 사용자 메시지 형식으로 보냅니다 계약\"".into(),
            "cli-tell".into(),
            Some("UI".into()),
        )]);
        let t = "[adash] 사용자 메시지 형식으로 보냅니다 계약";
        assert_eq!(h.classify("x", ("UI", None), 1_010_000, t, &mk).0, "agent");
        // the same text reaching a different agent is not that send
        assert_eq!(h.classify("y", ("CORE", None), 1_010_000, t, &mk).0, "human");
    }

    #[test]
    fn closed_agent_history_follows_its_session_and_a_reused_name_is_marked() {
        use crate::collect::{Agent, Snapshot};
        let _ = std::fs::remove_file(super::path());
        let mut h = super::History::default();
        let pane = "31aecb63:ea1b1edc";
        let file = std::env::temp_dir().join(format!("adash-test-sess-{}.jsonl", std::process::id()));
        std::fs::write(&file, "").unwrap();
        let fs = file.to_string_lossy().into_owned();
        let mk = |id: &str, pk: &str, name: &str| Agent {
            identity: id.into(),
            pane_key: pk.into(),
            name: name.into(),
            project: "galaxy-survey".into(),
            state: "done".into(),
            ..Default::default()
        };
        // first seen by pane (old identity rule), then the mapping says it is the session file
        h.observe(&Snapshot { at: 1_000, agents: vec![mk(pane, pane, "RS")], ..Default::default() });
        h.observe(&Snapshot { at: 2_000, agents: vec![mk(&fs, pane, "RS")], ..Default::default() });
        assert!(h.recs.iter().all(|r| r.identity == fs), "pane records moved to the session file");
        assert!(std::fs::read_to_string(super::path()).unwrap().lines().all(|l| !l.contains(pane)));
        // closed; a new agent takes the name RS
        h.observe(&Snapshot { at: 3_000, agents: vec![mk("/new/session.jsonl", "ce2e:210c", "RS")], ..Default::default() });
        let snap = Snapshot { at: 4_000, agents: vec![mk("/new/session.jsonl", "ce2e:210c", "RS")], ..Default::default() };
        let v = h.query(&snap, &crate::meta::Meta::default(), &[], 1.0, 4_000);
        let old = v["agents"].as_array().unwrap().iter().find(|a| a["identity"] == fs.as_str()).unwrap().clone();
        assert_eq!(old["live"], false);
        assert!(old["displayName"].as_str().unwrap().starts_with("RS (종료됨 "), "{old}");
        let _ = std::fs::remove_file(super::path());
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn closed_projects_are_known_and_test_sessions_leave_no_trace() {
        let rec = |id: &str, project: &str| super::Rec {
            ts: 1,
            identity: id.into(),
            name: "A".into(),
            project: project.into(),
            state: "offline".into(),
            prev_state: None,
        };
        let mut h = super::History {
            recs: vec![rec("/s/real.jsonl", "paper"), rec("/s/test.jsonl", "send_test"), rec("pane:t_feedbeef", "send_test")],
            ..Default::default()
        };
        // a project whose agents are all closed is still known (archive must not 404)
        assert!(h.has_project("paper") && h.has_project("send_test"));
        std::fs::write(std::env::temp_dir().join(format!("adash-test-handles-{}.txt", std::process::id())), "t_feedbeef\n").unwrap();
        h.purge_tests(&["/s/test.jsonl".to_string()]);
        assert!(!h.has_project("send_test"), "test sessions and their project are gone");
        assert!(h.has_project("paper"));
        let _ = std::fs::remove_file(std::env::temp_dir().join(format!("adash-test-handles-{}.txt", std::process::id())));
        let _ = std::fs::remove_file(super::test_ids_path());
    }
}
