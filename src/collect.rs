//! Join adash terminals (tmux panes) + /proc + session files into agents.
use crate::meta::Meta;
use crate::registry::Registry;
use crate::session::{self, clean_title, Progress, SessionCache};
use crate::term;
use crate::util::{now_ms, Res};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

static CODENAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b([A-Z][A-Z0-9]{1,7}):\s").unwrap());

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub name: String,
    pub name_source: String,
    pub ambiguous: bool,
    pub role: Option<String>,
    pub handle: Option<String>,
    pub pane_key: String,
    pub project: String,
    pub worktree_path: String,
    pub agent_type: String,
    pub model: Option<String>,
    /// normalized model (`analytics::norm_model`: no provider prefix / effort / date)
    pub model_norm: Option<String>,
    pub title: String,
    pub state: String,
    pub needs_human: bool,
    pub state_since: i64,
    pub waited_min: f64,
    /// prompt-cache countdown for agents waiting on the human: {ttlSec, expiresAt, state}
    pub cache: Option<Value>,
    /// last finished agent answer (ms), from the session record (or the waiting state's start)
    pub last_answer_at: Option<i64>,
    /// last human-typed message (ms), exact/title-mapped sessions only
    pub last_human_at: Option<i64>,
    /// "확인함" time (POST /api/agents/{name}/ack)
    pub acked_at: Option<i64>,
    /// an answer the human has not replied to or acknowledged yet
    pub unread: bool,
    /// unsent text in the agent's own input box: {text, readable, source, seenAt} (scanned ~10 s)
    pub draft: Option<Value>,
    /// login prompt on the agent's screen (state "auth"): {reason, phrase, url?, code?, since}
    pub auth: Option<Value>,
    /// input the harness holds until its turn ends: {phrase, sendNowKey, since}
    pub queued: Option<Value>,
    /// runs a harness version that has since been updated: {running, installed, why}
    pub outdated: Option<Value>,
    /// "정하실 것" items in the latest answer: [{id, text, options}]
    pub decisions: Value,
    /// hash of the message the decisions came from (changes when a new answer arrives)
    pub decisions_from: Option<String>,
    /// the human answered (message, ask answer, btw, dashboard send) or acknowledged after the
    /// decisions message; `decisions` is then empty until the agent writes new ones
    pub decisions_answered: bool,
    pub tool: Option<String>,
    pub prompt: String,
    pub last_message: String,
    pub progress: Option<Progress>,
    /// open question / permission prompt blocking on the human (state "asking")
    pub ask: Option<crate::ask::Ask>,
    pub snoozed_until: Option<i64>,
    /// its project is hidden (meta.yaml `projects.<name>.hidden`): not counted, not notified
    pub hidden: bool,
    /// state "error": the provider error that ended the last turn {status, message, provider, model, at}
    pub last_error: Option<crate::session::ErrInfo>,
    /// state "error": how the dashboard retries this harness {label, how}
    pub retry: Option<Value>,
    pub note: String,
    /// stable key: the session file (trusted mapping) or the terminal handle; same value as
    /// `/api/history` agents[].identity
    pub identity: String,
    /// a naming turn requested by adash: no notification, keeps queue position
    #[serde(skip)]
    pub quiet: bool,
    #[serde(skip)]
    pub base: String,
    #[serde(skip)]
    pub session_path: Option<std::path::PathBuf>,
    #[serde(skip)]
    pub mapping_exact: bool,
    /// exact | title | heuristic
    #[serde(skip)]
    pub mapping_confidence: &'static str,
    /// "transcript" (derived from the mapped session record) or "none" (no trusted mapping)
    #[serde(rename = "stateSource")]
    pub state_source: &'static str,
    /// working directory of the agent process (session cwd)
    #[serde(skip)]
    pub cwd: Option<std::path::PathBuf>,
}

/// A directory agents work in: a meta.yaml project path or a directory a terminal was opened in.
#[derive(Clone, Debug)]
pub struct Worktree {
    pub path: String,
    pub project: String,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub at: i64,
    pub agents: Vec<Agent>,
    pub worktrees: Vec<Worktree>,
}

pub struct Collector {
    /// false in CLI processes while the server is running (server is the single writer)
    pub persist: bool,
    pub cache: SessionCache,
    pub registry: Registry,
    /// (handle, asked_at) → question + options read off the screen (asks whose record has no options)
    pub screen_asks: HashMap<(String, i64), Option<Vec<crate::ask::AskQuestion>>>,
}

pub fn project_name(display: &str, path: &str) -> String {
    let base = Path::new(path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    if display.is_empty() || display == "main" || display == "master" {
        base
    } else {
        display.to_string()
    }
}

/// "DASH: Build agent progress dashboard - adash-dev" → (Some(DASH), "Build … dashboard")
pub fn split_title(raw: &str, project_tail: &[&str]) -> (Option<String>, String) {
    let t = clean_title(raw);
    let (code, mut rest) = match CODENAME.captures(&t) {
        Some(c) => {
            let m = c.get(0).unwrap();
            (Some(c[1].to_string()), t[m.end()..].trim().to_string())
        }
        None => (None, t.clone()),
    };
    if let Some((head, tail)) = rest.rsplit_once(" - ") {
        if !tail.contains(' ') || project_tail.contains(&tail) {
            rest = head.trim().to_string();
        }
    }
    (code, rest)
}

impl Collector {
    pub fn new() -> Collector {
        let mut registry = Registry::load();
        // names/acks of agents closed while keyed by pane move to their session file
        for (from, to) in crate::lifecycle::pane_rekeys() {
            registry.migrate(&from, &to);
        }
        Collector { cache: SessionCache::default(), registry, persist: true, screen_asks: HashMap::new() }
    }

    pub fn collect(&mut self, meta: &Meta) -> Res<Snapshot> {
        let now = now_ms();
        let panes = term::panes()?;
        let procs = session::scan_procs();
        // meta.yaml project of the directory, else the terminal's tmux session (= the project it
        // was opened for, e.g. a git worktree under ~/worktrees/<project>/), else the dir name
        let project_of =
            |p: &term::Pane| meta.project_for_path(&p.cwd).unwrap_or_else(|| if p.session.is_empty() { project_name("", &p.cwd) } else { p.session.clone() });

        // meta.yaml project paths, then the directories terminals were opened in
        let mut worktrees: Vec<Worktree> = Vec::new();
        for (name, pm) in &meta.projects {
            for p in &pm.paths {
                if !worktrees.iter().any(|w| &w.path == p) {
                    worktrees.push(Worktree { path: p.clone(), project: name.clone() });
                }
            }
        }
        for p in &panes {
            if !p.cwd.is_empty() && !worktrees.iter().any(|w| w.path.trim_end_matches('/') == p.cwd.trim_end_matches('/')) {
                worktrees.push(Worktree { path: p.cwd.clone(), project: project_of(p) });
            }
        }

        // terminal titles ("CODE: title") for session matching
        let mut titles = HashMap::new();
        for p in &panes {
            let (code, rest) = split_title(&p.title, &[]);
            if let Some(c) = code {
                titles.insert(p.handle.clone(), format!("{c}: {rest}"));
            }
        }
        let sessions = session::resolve_sessions(&procs, &titles, &mut self.cache);

        struct Raw {
            agent: Agent,
            code: Option<String>,
        }
        let mut raws: Vec<Raw> = Vec::new();
        for p in &panes {
            let handle = p.handle.clone();
            let proc_main = procs.get(&handle).and_then(|v| v.first());
            let Some(agent_type) = proc_main.map(|p| p.kind.clone()) else { continue }; // plain shell
            let wpath = p.cwd.clone();
            let project = project_of(p);
            let sess = sessions.get(&handle).cloned().unwrap_or_default();
            // trusted mapping (exact / title / pinned): the session record drives the agent's state
            let mapped = sess.exact;
            let (mut code, mut title) = split_title(&p.title, &[project.as_str(), "agy"]);
            if code.is_none() {
                if let Some(st) = &sess.title {
                    let (c2, t2) = split_title(st, &[]);
                    code = c2;
                    title = t2;
                }
            }
            let prompt = if mapped { sess.turn.prompt.clone().unwrap_or_default() } else { String::new() };
            // a shell title showing the launch command ("~/dir: agy --model …") is not a title
            let command_title = title.split_once(": ").is_some_and(|(_, c)| {
                c.split_whitespace().next().is_some_and(|w| ["agy", "claude", "codex", "omp", "bash", "exec"].contains(&w.rsplit('/').next().unwrap_or(w)))
            });
            if command_title {
                title = String::new();
            }
            if code.is_none() && (title.is_empty() || title == "ready" || title.contains(": ") || title.contains(" - ")) {
                title = if prompt.is_empty() { title } else { prompt_title(&prompt) };
            }
            let mut state = "idle".to_string();
            let mut state_since = if p.activity_ms > 0 { p.activity_ms } else { now };
            let mut state_source = "none";
            if let Some((open, since)) = sess.turn.phase.filter(|_| mapped) {
                state = if open { "working" } else { "done" }.into();
                state_since = since;
                state_source = "transcript";
            }
            let mut ask = derive_ask(&state, if mapped { sess.ask.clone() } else { None });
            // no options known in the record: read the Ask dialog off the screen, once per ask
            if let Some(q) = ask.as_mut().filter(|q| q.options.is_empty()) {
                if self.screen_asks.len() > 200 {
                    self.screen_asks.clear();
                }
                let key = (handle.clone(), q.asked_at);
                let parsed =
                    self.screen_asks.entry(key).or_insert_with(|| term::tail_n(&handle, 80).ok().and_then(|(_, t)| crate::ask::dialog_ask(&t))).clone();
                if let Some(qs) = parsed {
                    q.ask_kind = "question";
                    q.question = qs[0].question.clone();
                    q.options = qs[0].options.clone();
                    q.questions = qs;
                }
            }
            if let Some(q) = &ask {
                state = "asking".into();
                if q.asked_at > 0 {
                    state_since = q.asked_at;
                }
            }
            // the last turn ended on a provider error and nothing happened since: state "error"
            let last_error = if mapped { sess.turn.error.clone() } else { None }.filter(|_| state != "working" && state != "asking" && state != "idle");
            if let Some(e) = &last_error {
                state = "error".into();
                state_since = e.at;
            }
            // agy: the launch flag, else the model its conversation db records (restored agents run
            // as `agy --conversation <id>` only)
            let model = sess
                .model
                .clone()
                .or_else(|| proc_main.and_then(|p| session::arg_value(&p.args, "--model")))
                .or_else(|| (agent_type == "antigravity").then(|| sess.path.as_deref().and_then(crate::agy::model_of_cached)).flatten());
            // the session file is the agent's identity whenever the mapping is trusted (fd,
            // --resume, title; pinned per process): history, names and acks stay with the
            // session after the terminal closes. Guessed (mtime) mappings use the terminal handle.
            let identity = match (&sess.path, mapped) {
                (Some(f), true) => f.to_string_lossy().into_owned(),
                _ => handle.clone(),
            };
            raws.push(Raw {
                agent: Agent {
                    name: String::new(),
                    name_source: String::new(),
                    ambiguous: false,
                    role: None,
                    pane_key: handle.clone(),
                    handle: Some(handle),
                    worktree_path: wpath,
                    model,
                    model_norm: None,
                    title,
                    needs_human: false,
                    state_since,
                    waited_min: 0.0,
                    cache: None,
                    last_answer_at: if mapped { sess.turn.last_answer } else { None },
                    last_human_at: if mapped { sess.turn.last_human } else { None },
                    acked_at: None,
                    unread: false,
                    draft: None,
                    auth: None,
                    queued: None,
                    outdated: None,
                    decisions: Value::Null,
                    decisions_from: None,
                    decisions_answered: false,
                    tool: if mapped && state == "working" { sess.turn.tool.clone() } else { None },
                    state,
                    prompt,
                    last_message: if mapped { sess.turn.last_text.clone().unwrap_or_default() } else { String::new() },
                    progress: sess.progress.clone(),
                    ask,
                    snoozed_until: None,
                    hidden: false,
                    retry: last_error.as_ref().map(|_| crate::server::retry_info(&agent_type)),
                    last_error,
                    note: String::new(),
                    identity,
                    quiet: false,
                    base: format!("{project}-{agent_type}"),
                    session_path: sess.path.clone(),
                    cwd: proc_main.map(|p| p.cwd.clone()).filter(|c| !c.as_os_str().is_empty()),
                    mapping_exact: mapped,
                    mapping_confidence: if sess.path.is_some() { sess.confidence } else { "heuristic" },
                    state_source,
                    project,
                    agent_type,
                },
                code,
            });
        }
        raws.sort_by(|a, b| a.agent.pane_key.cmp(&b.agent.pane_key));
        for r in &raws {
            self.registry.migrate(&r.agent.pane_key, &r.agent.identity);
            // a name chosen at launch binds to the agent once its terminal shows up
            if let Some(n) = r.agent.handle.as_ref().and_then(|h| self.registry.pending_names.remove(h)) {
                self.registry.overrides.insert(r.agent.identity.clone(), n.name.clone());
                if n.role.is_some() {
                    self.registry.self_names.insert(r.agent.identity.clone(), n);
                }
            }
        }
        self.registry.prune(now);
        let wanted: Vec<(String, Option<String>, String, String)> =
            raws.iter().map(|r| (r.agent.identity.clone(), r.code.clone(), r.agent.base.clone(), r.agent.title.clone())).collect();
        let names = self.registry.names(&wanted);
        if self.persist {
            let _ = self.registry.save();
        }

        let hidden_projects = meta.hidden_projects(now);
        let mut agents: Vec<Agent> = raws
            .into_iter()
            .map(|r| {
                let mut a = r.agent;
                let (n, src, amb) = names[&a.identity].clone();
                a.name = n;
                a.name_source = src.to_string();
                a.ambiguous = amb;
                a.role = self.registry.self_names.get(&a.identity).and_then(|s| s.role.clone());
                a.snoozed_until = self.registry.snoozes.get(&a.identity).copied().filter(|u| *u > now);
                a.hidden = hidden_projects.contains(&a.project);
                a.note = meta.agents.get(&a.name).map(|m| m.note.clone()).unwrap_or_default();
                let waiting = a.state != "working" && a.state != "idle";
                a.needs_human = waiting && a.snoozed_until.is_none();
                // a naming turn we triggered: keep the previous waitingSince/queue position, stay quiet
                if let Some(req) = self.registry.name_requests.get(&a.identity) {
                    if let Some(sent) = req.sent_at {
                        let self_at = self.registry.self_names.get(&a.identity).map(|s| s.set_at);
                        let in_turn = a.state_since > sent
                            && a.state_since - sent < crate::registry::NAME_TURN_WINDOW_MS
                            && self_at.is_none_or(|t| t < sent || a.state_since <= t + 60_000);
                        if in_turn {
                            a.quiet = true;
                            if waiting {
                                a.state_since = req.prev_since;
                                a.needs_human = req.prev_needs_human && a.snoozed_until.is_none();
                            }
                        }
                    }
                }
                if waiting {
                    a.waited_min = ((now - a.state_since).max(0) as f64 / 60_000.0 * 10.0).round() / 10.0;
                    if a.last_answer_at.is_none() && a.state != "asking" {
                        a.last_answer_at = Some(a.state_since); // no answer time in the record: the waiting state's start
                    }
                    a.cache = Some(cache_view(&meta.cache_ttl, &a.agent_type, a.model.as_deref(), a.last_answer_at.unwrap_or(a.state_since), now));
                }
                a.model_norm = a.model.as_deref().map(crate::analytics::norm_model);
                let (d, from) = crate::decisions::for_message(&a.last_message);
                a.decisions = d;
                a.decisions_from = from;
                a.acked_at = self.registry.acked.get(&a.identity).copied();
                a.unread = a.last_answer_at.is_some_and(|ans| ans > a.last_human_at.unwrap_or(0).max(a.acked_at.unwrap_or(0)));
                a
            })
            .collect();
        self.name_from_launch(&mut agents);
        agents.sort_by(|a, b| a.project.cmp(&b.project).then(a.name.cmp(&b.name)));
        Ok(Snapshot { at: now, agents, worktrees })
    }
}

pub fn find<'a>(agents: &'a [Agent], key: &str) -> Option<&'a Agent> {
    agents
        .iter()
        .find(|a| a.name == key)
        .or_else(|| agents.iter().find(|a| a.handle.as_deref() == Some(key)))
        .or_else(|| agents.iter().find(|a| a.name.eq_ignore_ascii_case(key)))
        // a unique handle prefix ("t_17ed")
        .or_else(|| {
            let mut hits = agents.iter().filter(|a| key.starts_with("t_") && key.len() >= 6 && a.handle.as_deref().is_some_and(|h| h.starts_with(key)));
            let first = hits.next();
            if hits.next().is_some() {
                None
            } else {
                first
            }
        })
}

/// Caller's terminal handle: own env `ADASH_HANDLE`, else walk process ancestry.
/// Title from the first prompt: a thread marker `[marker] …` names it, else its first line.
fn prompt_title(prompt: &str) -> String {
    let p = prompt.trim_start();
    if let Some(m) = p.strip_prefix('[').and_then(|r| r.split_once(']')).map(|x| x.0.trim()).filter(|m| !m.is_empty() && !m.starts_with("adash")) {
        return m.to_string();
    }
    crate::util::first_line(p, 80)
}

impl Collector {
    /// Terminals started with `adash term new`: `--name` (pane option `@adash_name`), else the
    /// launching agent's name + the model family or harness (`PLANNER-argon`). Once per
    /// terminal, and only while the agent has no name of its own.
    fn name_from_launch(&mut self, agents: &mut [Agent]) {
        static DONE: std::sync::LazyLock<std::sync::Mutex<HashSet<String>>> = std::sync::LazyLock::new(Default::default);
        let by_handle: HashMap<String, String> = agents.iter().filter_map(|a| Some((a.handle.clone()?, a.name.clone()))).collect();
        let mut taken: HashSet<String> = agents.iter().map(|a| a.name.clone()).collect();
        for a in agents.iter_mut().filter(|a| a.name_source != "override" && a.name_source != "self") {
            let Some(h) = a.handle.clone() else { continue };
            if DONE.lock().unwrap_or_else(|e| e.into_inner()).contains(&h) {
                continue;
            }
            let name = term::pane_opt(&h, "@adash_name").or_else(|| {
                let parent = by_handle.get(&term::pane_opt(&h, "@adash_parent")?)?;
                let m = a.model.as_deref().unwrap_or("");
                let fam = ["argon", "astra", "sol", "luna", "opus", "sonnet", "codex"].iter().find(|f| m.contains(*f)).map(|f| f.to_string());
                Some(format!("{parent}-{}", fam.unwrap_or_else(|| a.agent_type.clone())))
            });
            // the launcher may show up a poll later: try again until there is a name or no hint
            if name.is_some() || term::pane_opt(&h, "@adash_parent").is_none() {
                DONE.lock().unwrap_or_else(|e| e.into_inner()).insert(h);
            }
            let Some(mut n) = name.map(|n| n.split_whitespace().collect::<Vec<_>>().join("-")) else { continue };
            let base = n.clone();
            let mut k = 2;
            while taken.contains(&n) {
                n = format!("{base}-{k}");
                k += 1;
            }
            taken.insert(n.clone());
            self.registry.overrides.insert(a.identity.clone(), n.clone());
            if self.persist {
                let _ = self.registry.save();
            }
            a.name = n;
            a.name_source = "override".into();
        }
    }
}

pub fn my_handle() -> Option<String> {
    if let Some(h) = term::env_handle() {
        return Some(h);
    }
    let mut pid = std::process::id();
    for _ in 0..32 {
        pid = session::ppid(pid)?;
        if pid <= 1 {
            return None;
        }
        if let Some(h) = session::env_of(pid, term::HANDLE_ENV).filter(|h| !h.is_empty()) {
            return Some(h);
        }
    }
    None
}

/// An agent blocked on the human: an unanswered `ask`/`AskUserQuestion` in its mapped session
/// (exact or title mapping) while its turn is open.
fn derive_ask(state: &str, session_ask: Option<crate::ask::Ask>) -> Option<crate::ask::Ask> {
    session_ask.filter(|_| state == "working")
}

/// Prompt-cache countdown from the last agent activity: warm (> 25 % of the TTL left),
/// cooling (last quarter), cold (expired).
pub fn cache_view(ttl: &crate::meta::CacheTtl, agent_type: &str, model: Option<&str>, base: i64, now: i64) -> Value {
    let (ttl_sec, key) = ttl.ttl(agent_type, model);
    let expires = base + ttl_sec as i64 * 1000;
    let left = expires - now;
    let state = if left <= 0 {
        "cold"
    } else if left * 4 <= ttl_sec as i64 * 1000 {
        "cooling"
    } else {
        "warm"
    };
    serde_json::json!({"ttlSec": ttl_sec, "expiresAt": expires, "state": state, "basis": crate::meta::ttl_basis(&key)})
}
