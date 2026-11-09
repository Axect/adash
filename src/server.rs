//! `adash serve`: HTTP API on 127.0.0.1, poller, notifier and relay loop.
use crate::collect::{find, Collector, Snapshot};
use crate::history::History;
use crate::meta::{HumanTask, Meta};
use crate::notify::Notifier;
use crate::paths::{percent_decode, Ctx, FileIndex};
use crate::registry::{validate_name, NameRequest, SelfName, NAME_REQUEST_COOLDOWN_MS};
use crate::threads::{self, Gate};
use crate::transcript::TranscriptCache;
use crate::util::LockExt;
use crate::util::{now_ms, Res};
use crate::{state, term};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, Server};

/// The whole `web/` tree (UI, vendored markdown-it/KaTeX, fonts) embedded at build time.
pub static WEB: include_dir::Dir<'static> = include_dir::include_dir!("$CARGO_MANIFEST_DIR/web");

/// Largest local file served by `/api/agents/{name}/file`.
const FILE_CAP: u64 = 25 * 1024 * 1024;

/// omp btw UI open per terminal handle: {open, btwId, since, followUp} (screen-checked every 10 s
/// for omp agents with a recent btw entry or a known open box).
pub static BTW_OPEN: LazyLock<Mutex<HashMap<String, Value>>> = LazyLock::new(Default::default);

fn btw_view(a: &crate::collect::Agent, follow_input: Option<bool>) -> Value {
    let newest = a.session_path.as_deref().map(crate::btw::load).and_then(|v| v.into_iter().last());
    match follow_input {
        Some(f) => json!({"open": true, "btwId": newest.as_ref().map(|e| e.id.clone()), "since": newest.as_ref().map(|e| e.created), "followUp": f}),
        None => json!({"open": false}),
    }
}

/// Re-check the btw box of omp agents that may have one open.
fn scan_btw(snap: &Snapshot) {
    let now = now_ms();
    for a in snap.agents.iter().filter(|a| a.agent_type == "omp") {
        let Some(h) = a.handle.clone() else { continue };
        let known_open = BTW_OPEN.plock().get(&h).is_some_and(|v| v["open"] == true);
        let recent = a
            .session_path
            .as_deref()
            .map(crate::btw::load)
            .and_then(|v| v.last().map(|e| e.updated.max(e.follow_ups.last().map(|f| f.updated).unwrap_or(0))))
            .is_some_and(|t| now - t < 6 * 3_600_000);
        if !known_open && !recent {
            BTW_OPEN.plock().remove(&h);
            continue;
        }
        let Ok(lines) = term::screen(&h) else { continue };
        let v = btw_view(a, crate::btw::screen_state(&lines));
        BTW_OPEN.plock().insert(h, v);
    }
}

/// Log provider transitions, notify, and after a recovery retry agents stopped by a network
/// error while it was down (once each, 3 s apart). Test overrides never notify or retry.
fn on_network_changes(sh: &Shared, changes: &[crate::network::Change], test_round: bool) {
    if changes.is_empty() {
        return;
    }
    let test = test_round || !crate::network::NET.plock().test_override.is_empty();
    if let Some(db) = sh.analytics.plock().as_ref() {
        for (p, from, to, _) in changes {
            db.log_action("network", p, &json!({"from": from, "to": to, "test": test}));
        }
    }
    if test {
        return;
    }
    let lost: Vec<&str> = changes.iter().filter(|c| c.2 != "ok").map(|c| crate::network::display(&c.0)).collect();
    if !lost.is_empty() {
        let all: Vec<&str> = crate::network::NET.plock().probes.values().filter(|p| p.state != "ok").map(|p| crate::network::display(&p.provider)).collect();
        crate::notify::send(&format!("네트워크 끊김: {} 접속 불가 (VPN 확인)", all.join("·")), "자동 전송은 접속이 돌아올 때까지 보류합니다.", None);
    }
    let back: Vec<(String, i64)> = changes.iter().filter(|c| c.2 == "ok" && c.1 != "ok").map(|c| (c.0.clone(), c.3)).collect();
    if back.is_empty() {
        return;
    }
    let auto = Meta::load().auto_retry_on_network_recovery.unwrap_or(true);
    let snap = sh.snap.plock().clone();
    let targets: Vec<crate::collect::Agent> = if auto {
        snap.agents
            .iter()
            .filter(|a| a.state == "error" && a.snoozed_until.is_none())
            .filter(|a| {
                let Some(e) = &a.last_error else { return false };
                let p = crate::network::provider_of(&a.agent_type, a.model.as_deref());
                back.iter().any(|(bp, since)| Some(bp.as_str()) == p && e.at >= since - 60_000) && crate::network::network_class(e.status, &e.message)
            })
            .cloned()
            .collect()
    } else {
        vec![]
    };
    let names: Vec<String> = back.iter().map(|b| crate::network::display(&b.0).to_string()).collect();
    crate::notify::send(&format!("네트워크 복구: 오류 에이전트 {}개 자동 재시도", targets.len()), &format!("{} 접속이 돌아왔습니다.", names.join("·")), None);
    if targets.is_empty() {
        return;
    }
    let Some(sh) = SHARED.get().cloned() else { return };
    std::thread::spawn(move || {
        for (i, a) in targets.iter().enumerate() {
            if i > 0 {
                std::thread::sleep(Duration::from_secs(3));
            }
            let r = op_retry(&sh, a);
            if let Some(db) = sh.analytics.plock().as_ref() {
                db.log_action("auto_retry", &a.name, &json!({"reason": "network_recovery", "ok": r.as_ref().is_ok_and(|v| v["resumed"] == true), "error": r.as_ref().err().map(|e| e.1.clone())}));
            }
        }
    });
}

/// Start the tmux server unit when the server is down (logged once per outage).
fn backend_watchdog() {
    static DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if term::status() {
        DOWN.store(false, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    let r = term::ensure_server();
    if !DOWN.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!("adash: tmux server down; started {}: {r:?}", term::UNIT);
    }
}

/// File types a phone may open on the desktop (`/open`).
const OPEN_FROM_DEVICE: &[&str] = &[".png", ".jpg", ".jpeg", ".gif", ".webp", ".svg", ".pdf", ".md", ".markdown", ".txt"];

/// /answer jobs (jobId → progress/result); kept for the server's lifetime (small).
static ANSWER_JOBS: std::sync::LazyLock<Mutex<HashMap<String, Value>>> = std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
/// The server state for worker threads started from request handlers.
static SHARED: std::sync::OnceLock<Arc<Shared>> = std::sync::OnceLock::new();

pub struct Shared {
    pub snap: Mutex<Snapshot>,
    pub coll: Mutex<Collector>,
    /// single writer for thread files and meta.yaml
    pub write: Mutex<()>,
    pub transcripts: Mutex<TranscriptCache>,
    pub history: Mutex<History>,
    /// worktree file lists for suffix path search (60 s TTL)
    pub files: Mutex<FileIndex>,
    /// analytics store for request-path writes (actions, snapshots) and /api/stats
    pub analytics: Mutex<Option<crate::analytics::Db>>,
    pub last_snapshot: std::sync::atomic::AtomicI64,
    pub workspaces: Mutex<crate::launch::Cached>,
    /// terminal handle → draft view from the last composer scan
    pub drafts: Mutex<HashMap<String, Value>>,
    /// handle → login prompt seen on its screen (with the time first seen)
    pub auth: Mutex<HashMap<String, Value>>,
    /// handle → input held by the harness until its turn ends
    pub queued: Mutex<HashMap<String, Value>>,
    pub catalog: Mutex<crate::launch::Cached>,
    /// Phase C: approved phones/tablets (tailscale serve), and the tailnet host/allowed logins
    pub devices: Mutex<crate::mobile::Devices>,
    pub ts_host: Option<String>,
    pub ts_allowed: Vec<String>,
    /// minute of the last cache prune
    pub last_prune: std::sync::atomic::AtomicI64,
}

/// After a launch: optional name/role for the new pane, then refresh state and caches.
fn after_launch(sh: &Shared, l: &crate::launch::Launched, j: &Value) -> Value {
    let mut body = l.body.clone();
    // `test: true` (throwaway sessions the dashboard's own agents launch): kept out of analytics awards
    if j["test"].as_bool() == Some(true) {
        if let Some(h) = body["handle"].as_str() {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(crate::analytics::data_dir().join("test_handles.txt")) {
                let _ = writeln!(f, "{h}");
            }
            body["test"] = json!(true);
        }
    }
    let name = j["name"].as_str().map(str::trim).filter(|n| !n.is_empty());
    let role = j["role"].as_str().map(str::trim).filter(|r| !r.is_empty());
    if let (Some(h), Some(n)) = (body["handle"].as_str(), name) {
        // the agent appears only once its session record maps to the pane, so the name waits
        // on the terminal handle and binds then
        let mut c = sh.coll.plock();
        c.registry.pending_names.insert(h.to_string(), crate::registry::SelfName { name: n.to_string(), role: role.map(String::from), set_at: now_ms() });
        let _ = c.registry.save();
        body["name"] = json!(n);
    }
    sh.workspaces.plock().invalidate();
    refresh(sh);
    body
}

/// Successful mutating request → `dashboard_actions` (bodies trimmed; chat text kept out, only sizes).
fn log_action(sh: &Shared, method: &Method, segs: &[&str], body: &str) {
    if matches!(method, Method::Get | Method::Head) {
        return;
    }
    let j: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let (action, target) = match segs {
        ["api", "agents", name, action] => (action.to_string(), name.to_string()),
        ["api", "threads", id, action] => (action.to_string(), id.to_string()),
        ["api", rest @ ..] => (rest.join("/"), j["to"].as_str().unwrap_or("").to_string()),
        _ => return,
    };
    let detail = match action.as_str() {
        "send" => {
            json!({"chars": j["text"].as_str().map(|t| t.chars().count()), "images": j["images"].as_array().map(|a| a.len()), "files": j["files"].as_array().map(|a| a.len()), "force": j["force"], "channel": j["channel"]})
        }
        "ask" => json!({"chars": j["request"].as_str().map(|t| t.chars().count()), "from": j["from"]}),
        // terminal input: only its size and named key, never the text (it can be a password or a code)
        "term-input" => json!({"chars": j["text"].as_str().map(|t| t.chars().count()), "key": j["key"]}),
        "stat" => return, // read-only lookups fired per message render
        "ack" => json!({"method": method.to_string()}),
        _ => j,
    };
    if let Some(db) = sh.analytics.plock().as_ref() {
        db.log_action(&action, &target, &detail);
    }
}

// ---------- operations shared by HTTP handlers and direct CLI mode ----------

pub fn op_rename(coll: &mut Collector, snap: &Snapshot, key: &str, new: &str) -> Res<Value> {
    let new = new.trim();
    if new.is_empty() || new.contains(char::is_whitespace) {
        return Err("name must be non-empty without spaces".into());
    }
    let a = find(&snap.agents, key).ok_or_else(|| format!("unknown agent {key}"))?;
    if snap.agents.iter().any(|o| o.name == new && o.identity != a.identity) {
        return Err(format!("name {new} already in use"));
    }
    coll.registry.overrides.insert(a.identity.clone(), new.to_string());
    coll.registry.save()?;
    let mirrored = match &a.handle {
        Some(h) => term::set_title(h, &format!("{new}: {}", a.title)).map(|_| true).unwrap_or(false),
        None => false,
    };
    Ok(json!({"ok": true, "old": a.name, "name": new, "mirroredToTerminal": mirrored}))
}

/// Longest snooze: 30 days, in minutes.
pub const MAX_SNOOZE_MIN: f64 = 43_200.0;

pub fn op_snooze(coll: &mut Collector, snap: &Snapshot, key: &str, minutes: f64) -> Res<Value> {
    let a = find(&snap.agents, key).ok_or_else(|| format!("unknown agent {key}"))?;
    let minutes = minutes.min(MAX_SNOOZE_MIN);
    let until = if minutes <= 0.0 {
        coll.registry.snoozes.remove(&a.identity);
        None
    } else {
        let u = now_ms() + (minutes * 60_000.0) as i64;
        coll.registry.snoozes.insert(a.identity.clone(), u);
        Some(u)
    };
    coll.registry.save()?;
    Ok(json!({"ok": true, "name": a.name, "snoozedUntil": until}))
}

pub fn op_focus(snap: &Snapshot, key: &str) -> Res<Value> {
    let a = find(&snap.agents, key).ok_or_else(|| format!("unknown agent {key}"))?;
    let h = a.handle.clone().ok_or("agent has no terminal")?;
    let r = term::focus(&h)?;
    Ok(json!({"ok": true, "name": a.name, "handle": h, "opened": r.get("opened").cloned()}))
}

/// Human chat message / `adash tell`: single line, bracketed paste (omp) or inbox file.
/// Never queued: a closed gate is reported as `held` so the caller can retry with force.
/// `force` skips the composer check and allows steering a working agent.
/// How each harness retries a turn that ended on a provider error. omp binds "Retry last
/// failed assistant turn" (`app.retry`) to F5 and Alt+R; claude, codex and agy have no retry
/// key, so they get a short "continue" message.
pub fn retry_mode(agent_type: &str) -> &'static str {
    match crate::harness::get(agent_type).map(|h| h.retry()) {
        Some(crate::harness::Retry::Key(..)) => "key",
        _ => "message",
    }
}

pub const RETRY_MESSAGE: &str = crate::harness::CONTINUE;

/// `agent.retry` for the UI: button label and what it does.
pub fn retry_info(agent_type: &str) -> Value {
    if retry_mode(agent_type) == "key" {
        json!({"label": "F5 재시도", "how": "omp 의 F5(마지막 실패 턴 재시도)를 보냅니다"})
    } else {
        json!({"label": format!("재시도 ({agent_type})"), "how": format!("{agent_type} 에는 재시도 키가 없어 '{RETRY_MESSAGE}' 메시지를 보냅니다")})
    }
}

/// Retry an agent stopped by a provider error, then watch up to 20 s for it to leave "error".
fn op_retry(sh: &Shared, a: &crate::collect::Agent) -> Result<Value, (u16, String)> {
    if a.state != "error" {
        return Err((409, format!("{} is not stopped on an error (state {})", a.name, a.state)));
    }
    let h = a.handle.clone().ok_or((409, "agent has no terminal".to_string()))?;
    let mode = retry_mode(&a.agent_type);
    // omp's retry clears the input box: never with the user's unsent text in it
    if a.draft.as_ref().is_some_and(|d| d["text"].as_str().is_some_and(|t| !t.trim().is_empty())) {
        return Err((409, format!("{} 입력창에 보내지 않은 초안이 있어 재시도하지 않았습니다", a.name)));
    }
    let lk = term::send_lock(&h);
    {
        let _g = lk.plock();
        let r = match crate::harness::get(&a.agent_type).map(|x| x.retry()) {
            Some(crate::harness::Retry::Key(seq, _)) => term::send_text(&h, seq),
            _ => term::send_line(&h, RETRY_MESSAGE),
        };
        r.map_err(|e| (502, format!("send failed: {e}")))?;
    }
    let t0 = now_ms();
    let mut state = a.state.clone();
    let mut err = a.last_error.clone();
    while now_ms() - t0 < 20_000 {
        std::thread::sleep(Duration::from_secs(2));
        refresh(sh);
        if let Some(x) = sh.snap.plock().agents.iter().find(|x| x.identity == a.identity) {
            state = x.state.clone();
            err = x.last_error.clone();
        }
        if state != "error" {
            break;
        }
    }
    let resumed = state != "error";
    let error = (!resumed).then(|| json!({"status": err.as_ref().and_then(|e| e.status), "message": err.as_ref().map(|e| e.message.clone())}));
    Ok(
        json!({"name": a.name, "resumed": resumed, "state": state, "error": error, "mode": mode, "sent": match crate::harness::get(&a.agent_type).map(|x| x.retry()) { Some(crate::harness::Retry::Key(_, name)) => name, _ => RETRY_MESSAGE }, "elapsedMs": now_ms() - t0}),
    )
}

/// A btw question: a follow-up when omp's btw box is open ("f" opens the follow-up input,
/// verified), else "/btw <text>". Screen-checked before every key.
fn op_btw(a: &crate::collect::Agent, text: &str) -> Result<Value, (u16, String)> {
    if a.agent_type != "omp" {
        return Err((501, "btw is an omp feature".into()));
    }
    let text = text.replace(['\r', '\n'], " ");
    let text = text.trim();
    if text.is_empty() {
        return Err((400, "text required".into()));
    }
    let h = a.handle.clone().ok_or((409, "agent has no terminal".to_string()))?;
    let lk = term::send_lock(&h);
    let _g = lk.plock();
    let screen = term::screen(&h).map_err(|e| (502, e))?;
    let state = crate::btw::screen_state(&screen);
    let raw = |t: &str| term::send_text(&h, t).map(|_| ()).map_err(|e| (502, e));
    match state {
        Some(follow_input) => {
            if !follow_input {
                raw("f")?;
                std::thread::sleep(Duration::from_millis(800));
                let s2 = term::screen(&h).map_err(|e| (502, e))?;
                if crate::btw::screen_state(&s2) != Some(true) {
                    return Ok(
                        json!({"ok": false, "delivered": false, "held": "btw_followup_unavailable", "reason": "follow-up input did not open (the answer may still be running)"}),
                    );
                }
            }
            term::send_line(&h, text).map_err(|e| (502, e))?;
            BTW_OPEN.plock().insert(h, btw_view(a, Some(false)));
            Ok(json!({"ok": true, "delivered": true, "followUp": true, "btwId": btw_view(a, Some(false))["btwId"]}))
        }
        None => {
            if a.state == "asking" {
                return Ok(json!({"ok": false, "delivered": false, "held": "asking"}));
            }
            term::send_line(&h, &format!("/btw {text}")).map_err(|e| (502, e))?;
            BTW_OPEN.plock().insert(h, json!({"open": true, "btwId": null, "since": now_ms(), "followUp": false}));
            Ok(json!({"ok": true, "delivered": true, "followUp": false, "kind": "btw", "btwText": text}))
        }
    }
}

/// Close omp's btw box with Esc, only when the screen shows it open (Esc would otherwise
/// interrupt the agent's turn).
fn op_btw_close(a: &crate::collect::Agent) -> Result<Value, (u16, String)> {
    if a.agent_type != "omp" {
        return Err((501, "btw is an omp feature".into()));
    }
    let h = a.handle.clone().ok_or((409, "agent has no terminal".to_string()))?;
    let lk = term::send_lock(&h);
    let _g = lk.plock();
    let screen = term::screen(&h).map_err(|e| (502, e))?;
    if crate::btw::screen_state(&screen).is_none() {
        BTW_OPEN.plock().insert(h, json!({"open": false}));
        return Ok(json!({"ok": true, "wasOpen": false}));
    }
    term::send_text(&h, "\x1b").map_err(|e| (502, e))?;
    std::thread::sleep(Duration::from_millis(800));
    let closed = term::screen(&h).map(|s| crate::btw::screen_state(&s).is_none()).unwrap_or(false);
    BTW_OPEN.plock().insert(h, json!({"open": !closed}));
    Ok(json!({"ok": closed, "wasOpen": true}))
}

/// Project actions: hide (until ms, or until unhidden), unhide, close every live agent,
/// archive (close + hide for good, remembering the sessions), restore (unhide), reopen-all.
/// Nothing deletes files, sessions or analytics.
fn op_project(sh: &Shared, name: &str, action: &str, j: &Value) -> Result<Value, (u16, Value)> {
    let bad = |c: u16, e: String| (c, json!({"error": e}));
    let snap = sh.snap.plock().clone();
    // a project is known from its settings, a live agent, or a closed agent still on the board
    let known = Meta::load().projects.contains_key(name) || snap.agents.iter().any(|a| a.project == name) || sh.history.plock().has_project(name);
    if !known {
        return Err(bad(404, format!("unknown project {name}")));
    }
    let set_hidden = |until: Option<Option<i64>>,
                      archived: Option<bool>,
                      sessions: Option<Vec<crate::meta::ArchivedSession>>|
     -> Result<crate::meta::ProjectMeta, (u16, Value)> {
        let _w = sh.write.plock();
        let mut m = Meta::load();
        let p = m.projects.entry(name.to_string()).or_default();
        if let Some(u) = until {
            p.hidden = Some(crate::meta::Hidden { until: u });
        } else if archived == Some(false) {
            p.hidden = None;
        }
        if let Some(a) = archived {
            p.archived = a;
        }
        if let Some(s) = sessions {
            p.archived_sessions = s;
        }
        let out = p.clone();
        m.save().map_err(|e| bad(500, e))?;
        Ok(out)
    };
    // close every live agent; agents that are working / asking / have unsent text need to be
    // named in `confirm` (or mode "force")
    let close_all = |j: &Value| -> Result<Vec<Value>, (u16, Value)> {
        let mode = j["mode"].as_str().unwrap_or("graceful");
        let confirmed: Vec<&str> = j["confirm"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).collect();
        let agents: Vec<crate::collect::Agent> = snap.agents.iter().filter(|a| a.project == name).cloned().collect();
        let needs: Vec<Value> = agents
            .iter()
            .filter_map(|a| {
                let draft = a.draft.as_ref().and_then(|d| d["text"].as_str()).filter(|t| !t.trim().is_empty());
                let why = if a.state == "working" || a.state == "asking" {
                    Some(a.state.as_str())
                } else if draft.is_some() {
                    Some("draft")
                } else {
                    None
                };
                why.filter(|_| mode != "force" && !confirmed.contains(&a.name.as_str())).map(|w| json!({"name": a.name, "why": w, "draft": draft}))
            })
            .collect();
        if !needs.is_empty() {
            return Err((409, json!({"error": "confirm", "reason": "confirm", "project": name, "agents": agents.len(), "needs": needs})));
        }
        let body = json!({"mode": mode, "confirmWorking": true, "discardDraft": true});
        Ok(agents
            .iter()
            .map(|a| match crate::lifecycle::close(&snap, a, &body) {
                Ok(mut v) => {
                    v["identity"] = json!(a.identity);
                    v
                }
                Err((code, mut v)) => {
                    v["name"] = json!(a.name);
                    v["ok"] = json!(false);
                    v["code"] = json!(code);
                    v
                }
            })
            .collect())
    };
    match action {
        "hide" => {
            let until = j["until"].as_i64().filter(|u| *u > now_ms());
            let p = set_hidden(Some(until), None, None)?;
            Ok(json!({"ok": true, "project": name, "hidden": p.hidden}))
        }
        "unhide" | "restore" => {
            let p = set_hidden(None, Some(false), None)?;
            Ok(json!({"ok": true, "project": name, "hidden": null, "archived": false, "archivedSessions": p.archived_sessions}))
        }
        "close" => {
            let results = close_all(j)?;
            Ok(json!({"ok": results.iter().all(|r| r["ok"] == true), "project": name, "results": results}))
        }
        "archive" => {
            let results = close_all(j)?;
            let sessions: Vec<crate::meta::ArchivedSession> = results
                .iter()
                .filter(|r| r["ok"] == true && r["reopenable"] == true)
                .filter_map(|r| Some(crate::meta::ArchivedSession { name: r["name"].as_str()?.to_string(), identity: r["identity"].as_str()?.to_string() }))
                .collect();
            set_hidden(Some(None), Some(true), Some(sessions.clone()))?;
            Ok(json!({"ok": results.iter().all(|r| r["ok"] == true), "project": name, "results": results, "archivedSessions": sessions}))
        }
        "reopen-all" => {
            let m = Meta::load();
            let sessions = m.projects.get(name).map(|p| p.archived_sessions.clone()).unwrap_or_default();
            let results: Vec<Value> = sessions
                .iter()
                .map(|s| match crate::lifecycle::reopen(&sh.snap.plock().clone(), &s.identity) {
                    Ok(v) => v,
                    Err((code, mut v)) => {
                        v["name"] = json!(s.name);
                        v["ok"] = json!(false);
                        v["code"] = json!(code);
                        v
                    }
                })
                .collect();
            Ok(json!({"ok": results.iter().all(|r| r["ok"] == true), "project": name, "results": results}))
        }
        _ => Err(bad(404, format!("unknown action {action}"))),
    }
}

/// `key` by name must name exactly one live agent (two panes with the same name: use the handle).
fn find_target<'a>(snap: &'a Snapshot, key: &str) -> Res<&'a crate::collect::Agent> {
    let a = find(&snap.agents, key).ok_or_else(|| format!("unknown agent {key}"))?;
    let same = snap.agents.iter().filter(|x| x.name == a.name).count();
    if same > 1 && a.handle.as_deref() != Some(key) {
        return Err(format!("not delivered ({key}): the name is ambiguous; use the terminal handle {}", a.handle.as_deref().unwrap_or("?")));
    }
    Ok(a)
}

pub fn op_send(snap: &Snapshot, key: &str, text: &str, images: &[String], force: bool) -> Res<Value> {
    op_send_files(snap, key, text, images, &[], force)
}

/// `op_send` with attached files (adash uploads), mentioned per harness after the text.
pub fn op_send_files(snap: &Snapshot, key: &str, text: &str, images: &[String], files: &[String], force: bool) -> Res<Value> {
    let a = find_target(snap, key)?;
    let lk = term::send_lock(a.handle.as_deref().unwrap_or(""));
    let _g = lk.plock();
    let imgs = images.iter().map(|p| crate::uploads::check_upload_path(p)).collect::<Res<Vec<_>>>()?;
    let fls = files.iter().map(|p| crate::uploads::check_upload_path(p)).collect::<Res<Vec<_>>>()?;
    let composed = crate::uploads::compose_all(text, &imgs, &fls, &a.agent_type);
    let text = composed.trim_end_matches(['\n', '\r', ' ']).trim_start_matches(['\n', '\r']);
    if text.trim().is_empty() {
        return Err("empty text".into());
    }
    let multi = text.contains('\n');
    let mode = match (multi, a.agent_type.as_str()) {
        (false, _) => "line",
        (true, "omp") => "paste",
        (true, _) => "file",
    };
    let held = |r: &str| Ok(json!({"ok": false, "delivered": false, "held": r, "mode": mode, "file": null, "name": a.name}));
    let held_why = |r: &str, why: &str| Ok(json!({"ok": false, "delivered": false, "held": r, "detail": why, "mode": mode, "file": null, "name": a.name}));
    if a.state == "asking" {
        return held("asking");
    }
    if a.state == "trust" {
        // the harness waits for "trust this folder?": keep the message and deliver it after
        if let Some(h) = &a.handle {
            TRUST_QUEUE.plock().entry(h.clone()).or_default().push(text.to_string());
        }
        return Ok(
            json!({"ok": false, "delivered": false, "held": "trust", "queued": true, "detail": "waiting for the folder to be trusted; sent once it is", "mode": mode, "file": null, "name": a.name}),
        );
    }
    if a.state == "working" && !force {
        return held("working");
    }
    if !force {
        match threads::gate(a) {
            Gate::Ok => {}
            Gate::Busy(r) if r.contains("working") => return held("working"),
            Gate::Busy(r) if r.contains("draft") => {
                snapshot_of(a, "send-held-draft");
                return held_why("possible_draft", &r);
            }
            Gate::Busy(r) => {
                snapshot_of(a, "send-refused");
                return Err(format!("not delivered ({}): {r}", a.name));
            }
            Gate::Held(r) => {
                snapshot_of(a, "send-held");
                return held_why("possible_draft", &r);
            }
        }
    }
    let h = a.handle.clone().ok_or("agent has no terminal")?;
    let mut file = None;
    let sent_at = now_ms();
    let mut typed = text.to_string();
    let r = match mode {
        "line" => term::send_line(&h, text)?,
        "paste" => {
            let r = term::send_paste(&h, text)?;
            // the Enter after a paste must submit: the input box empties (omp, claude, codex)
            if matches!(a.agent_type.as_str(), "omp" | "claude" | "codex") && !submitted(&a.agent_type, &h) {
                term::send_keys(&h, &["Enter"])?;
                if !submitted(&a.agent_type, &h) {
                    term::snapshot(&h, "paste-not-submitted");
                    return Ok(
                        json!({"ok": false, "delivered": false, "held": "not_submitted", "detail": "the text is in the agent's input box but Enter did not send it; send or clear it from the draft bar or the terminal", "mode": mode, "file": null, "name": a.name}),
                    );
                }
            }
            r
        }
        _ => {
            let dir = crate::platform::data_dir().join("inbox");
            let safe: String = a.name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
            let p = dir.join(format!("{safe}-{}.md", chrono::Local::now().format("%Y%m%d-%H%M%S")));
            crate::util::atomic_write(&p, text.as_bytes())?;
            typed = format!("[adash] 사용자 메시지(여러 줄): {} 를 읽고 그 지시를 따르세요.", p.display());
            let r = term::send_line(&h, &typed)?;
            file = Some(p.to_string_lossy().into_owned());
            r
        }
    };
    // the client matches its pending bubble against the transcript: the time typing started and the
    // text the agent's session will record (the pointer line for a file-mode message). The session's
    // message id does not exist yet when this returns.
    Ok(json!({"ok": true, "delivered": true, "held": null, "mode": mode, "file": file, "name": a.name, "receipt": r, "sentAt": sent_at, "typedText": typed}))
}

/// The agent's input box reads empty within ~3 s (a submitted message left it).
fn submitted(agent_type: &str, h: &str) -> bool {
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(300));
        let r = term::screen(h)
            .map(|t| ("screen".to_string(), t))
            .or_else(|_| term::tail(h))
            .ok()
            .and_then(|(src, tail)| threads::composer_text(agent_type, &src, &tail));
        if r == Some(None) {
            return true;
        }
    }
    false
}

/// Keep the agent's screen for diagnosis when a delivery was held or refused.
fn snapshot_of(a: &crate::collect::Agent, reason: &str) {
    if let Some(h) = &a.handle {
        term::snapshot(h, reason);
    }
}

/// Parse one answer object: `{optionIndex}` | `{optionIndices}` | `{text}`.
fn parse_answer(j: &Value) -> Option<crate::ask::Answer> {
    use crate::ask::Answer;
    if let Some(t) = j["text"].as_str().filter(|t| !t.trim().is_empty()) {
        Some(Answer::Text(t.to_string()))
    } else if let Some(arr) = j["optionIndices"].as_array() {
        Some(Answer::Many(arr.iter().filter_map(|x| x.as_u64().map(|n| n as usize)).collect()))
    } else {
        j["optionIndex"].as_u64().map(|i| Answer::One(i as usize))
    }
}

/// Answer an open omp `ask` (one or several questions) by driving its TUI.
/// Per question: read the box (must show that question), move from the visible cursor
/// (it starts on a recommended option), Enter selects (single) / Space toggles + Enter
/// (multi) / "Other" + typed text; Enter moves to the next question. Several questions end on
/// a "Review answers" page that is checked against the request before the final Enter.
/// Verified when the toolResult records every answer (≤15 s). 409 whenever the UI can't be
/// driven reliably (the UI then offers to show the terminal); partial answers are 400.
/// Body: `{answers: [{id, optionIndex | optionIndices | text}]}` or, for one question,
/// `{optionIndex | optionIndices | text}`.
/// Answer an open ask (all questions). `progress(stage, detail)` reports keys k/N, review,
/// verifying. Whatever happens while driving the dialog, the session's toolResult decides: an
/// answer that was recorded is a success even if a screen check failed afterwards.
pub fn op_answer(a: &crate::collect::Agent, j: &Value, progress: &dyn Fn(&str, Value)) -> Result<Value, (u16, String)> {
    use crate::ask::{current_question, excerpt, keys, result_matches, review_answers, review_matches, review_text, screen, Answer, Screen};
    let conflict = |m: &str| (409u16, m.to_string());
    let ask = a.ask.clone().ok_or_else(|| conflict("no open question"))?;
    if ask.ask_kind != "question" || ask.call_id.is_empty() {
        return Err(conflict("question is not mapped to a session record (or is a permission prompt); answer in the terminal"));
    }
    if a.agent_type != "omp" {
        return Err(conflict("answering is only supported for omp; answer in the terminal"));
    }
    let qs = &ask.questions;
    // answers in question order; every question exactly once
    let answers: Vec<Answer> = match j["answers"].as_array() {
        Some(list) => {
            let mut out = Vec::new();
            for q in qs {
                let item = list
                    .iter()
                    .find(|x| x["id"].as_str() == Some(q.id.as_str()))
                    .ok_or((400, format!("missing answer for question '{}' (partial answers are not allowed)", q.id)))?;
                out.push(parse_answer(item).ok_or((400, format!("answer for '{}' needs optionIndex, optionIndices or text", q.id)))?);
            }
            if list.len() != qs.len() {
                return Err((400, format!("{} answers for {} questions", list.len(), qs.len())));
            }
            out
        }
        None if qs.len() == 1 => vec![parse_answer(j).ok_or((400, "optionIndex, optionIndices or text required".to_string()))?],
        None => return Err((400, format!("this ask has {} questions: send answers: [{{id, …}}] for all of them", qs.len()))),
    };
    let h = a.handle.clone().ok_or_else(|| conflict("agent has no terminal"))?;
    let session = a.session_path.clone().ok_or_else(|| conflict("no session file"))?;
    let send = |text: &str, enter: bool| -> Result<(), (u16, String)> {
        term::send_text(&h, text).map_err(|e| (502, e))?;
        if enter {
            term::send_enter(&h).map_err(|e| (502, e))?;
            if !submitted(&a.agent_type, &h) {
                term::send_enter(&h).map_err(|e| (502, e))?;
                if !submitted(&a.agent_type, &h) {
                    term::snapshot(&h, "draft-submit-failed");
                    return Err((409, "the draft was not sent (Enter left it in the input box); use the terminal".into()));
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(if enter { 600 } else { 120 }));
        Ok(())
    };
    // the rendered frame of a background pane can lag a keystroke: take a frame only when two
    // reads 120 ms apart agree
    let look = || -> Result<(Screen, Vec<String>), (u16, String)> {
        // compare the parsed dialog, not the raw frame: the status-line spinner changes every read
        let mut prev = term::screen(&h).map_err(|e| (502, e))?;
        let mut prev_sc = screen(&prev);
        for _ in 0..8 {
            std::thread::sleep(std::time::Duration::from_millis(120));
            let cur = term::screen(&h).map_err(|e| (502, e))?;
            let cur_sc = screen(&cur);
            let same = cur_sc == prev_sc;
            prev = cur;
            prev_sc = cur_sc;
            if same {
                break;
            }
        }
        Ok((prev_sc, prev))
    };
    let stuck = |what: String, sc: &Screen, tail: &[String]| {
        term::snapshot(&h, "answer-stuck");
        (409u16, format!("{what}\n--- 터미널 화면 ---\n{}", excerpt(sc, tail)))
    };
    // plan every question before typing anything (bad indices fail early with 400)
    for (q, ans) in qs.iter().zip(&answers) {
        keys(q, 0, ans).map_err(|e| (400, format!("{}: {e}", q.id)))?;
    }
    let n = qs.len();
    let tabs = n > 1 || qs.iter().any(|q| q.multi);
    let steps = (|| -> Result<(), (u16, String)> {
        progress("focusing", json!({"k": 0, "n": n}));
        // 1. get the dialog in front: close a text prompt left open (Esc closes only the prompt),
        //    show a hidden dialog (Alt+H)
        let mut ready = false;
        for _ in 0..4 {
            let (sc, tail) = look()?;
            match sc {
                Screen::Dialog { .. } => {
                    ready = true;
                    break;
                }
                Screen::Prompt { .. } => send("\x1b", false)?,
                Screen::Hidden => send("\x1bh", false)?,
                Screen::None => return Err(stuck("질문 창이 터미널에 보이지 않습니다; 터미널에서 직접 답해 주세요".into(), &sc, &tail)),
            }
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
        if !ready {
            let (sc, tail) = look()?;
            return Err(stuck("질문 창을 앞으로 가져오지 못했습니다(입력 칸/숨김 상태)".into(), &sc, &tail));
        }
        // where the dialog is: question index, n = review (Submit tab)
        let locate = |sc: &Screen| -> Option<usize> {
            match sc {
                Screen::Dialog { review: true, .. } => Some(n),
                Screen::Dialog { title, .. } => current_question(title, qs),
                _ => None,
            }
        };
        // 2a. fast path: one batched send per step, one frame read per check. Any surprise
        //     (frame not as expected, review page differs) falls through to the careful
        //     key-by-key path below, which starts from whatever state the dialog is in.
        let raw = |text: &str| -> Result<(), (u16, String)> { term::send_text(&h, text).map(|_| ()).map_err(|e| (502, e)) };
        // read frames until `ok` holds (the frame can lag the keys), at most ~1.2 s
        let wait_for = |ok: &dyn Fn(&Screen) -> bool| -> Result<(Screen, Vec<String>, bool), (u16, String)> {
            let mut last = (Screen::None, vec![]);
            for k in 0..12 {
                if k > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                let tail = term::screen(&h).map_err(|e| (502, e))?;
                let sc = screen(&tail);
                if ok(&sc) {
                    return Ok((sc, tail, true));
                }
                last = (sc, tail);
            }
            Ok((last.0, last.1, false))
        };
        let fast = || -> Result<bool, (u16, String)> {
            for (i, (q, ans)) in qs.iter().zip(&answers).enumerate() {
                progress(if matches!(ans, Answer::Text(_)) { "typing" } else { "selecting" }, json!({"question": q.id, "k": i, "n": n}));
                let (sc, _, _) = wait_for(&|sc| locate(sc) == Some(i))?;
                let Some(cur) = locate(&sc) else { return Ok(false) };
                // tabs to this question + cursor to the first option, one send
                let mut keys_s = "\t".repeat((i + n + 1 - cur) % (n + 1));
                keys_s.push_str(&"\x1b[A".repeat(q.options.len() + 2));
                raw(&keys_s)?;
                let first = q.options.first().map(|o| o.label.clone()).unwrap_or_default();
                let (sc, _, ok) = wait_for(&|sc| {
                    locate(sc) == Some(i)
                        && matches!(sc, Screen::Dialog { lines, .. } if crate::ask::cursor_label(lines).is_some_and(|l| crate::ask::label_matches(&l, &first)))
                })?;
                if !ok {
                    return Ok(false);
                }
                let Screen::Dialog { lines, .. } = &sc else { return Ok(false) };
                match ans {
                    Answer::Many(want) => {
                        // checkbox state of the visible rows (others assumed unchecked; the review page checks)
                        let checked: Vec<bool> = q
                            .options
                            .iter()
                            .map(|o| {
                                lines.iter().any(|l| {
                                    (l.contains('\u{f14a}') || l.contains('\u{f046}'))
                                        && crate::ask::label_matches(
                                            l.trim_start_matches('\u{f054}').trim().trim_start_matches(['\u{f14a}', '\u{f046}']).trim(),
                                            &o.label,
                                        )
                                })
                            })
                            .collect();
                        let mut b = String::new();
                        for idx in 0..q.options.len() {
                            if checked[idx] != want.contains(&idx) {
                                b.push(' ');
                            }
                            if idx + 1 < q.options.len() {
                                b.push_str("\x1b[B");
                            }
                        }
                        b.push('\r');
                        raw(&b)?;
                    }
                    Answer::One(k) => raw(&format!("{}\r", "\x1b[B".repeat(*k)))?,
                    Answer::Text(t) => {
                        raw(&format!("{}\r", "\x1b[B".repeat(q.options.len())))?;
                        let (_, _, ok) = wait_for(&|sc| matches!(sc, Screen::Prompt { custom: true, .. }))?;
                        if !ok {
                            return Ok(false);
                        }
                        raw(&format!("\x15\x15\x15{}\r", t.replace(['\r', '\n'], " ")))?;
                        if q.multi {
                            // custom text leaves a multi question on its tab
                            let (sc, _, _) = wait_for(&|sc| matches!(sc, Screen::Dialog { .. }))?;
                            if locate(&sc) == Some(i) {
                                raw("\t")?;
                            }
                        }
                    }
                }
            }
            if n == 1 {
                return Ok(true); // a single question submits on its own; the toolResult decides
            }
            progress("submitting", json!({"k": n, "n": n}));
            let (sc, _, ok) = wait_for(&|sc| matches!(sc, Screen::Dialog { review: true, .. }))?;
            let Screen::Dialog { review: true, lines, .. } = &sc else { return Ok(false) };
            let _ = ok;
            let got = review_answers(lines, qs);
            if qs.iter().zip(&answers).enumerate().any(|(k, (q, a))| got[k].as_deref().is_some_and(|g| !review_matches(g, &review_text(q, a)))) {
                return Ok(false);
            }
            raw("\r")?;
            Ok(true)
        };
        if fast()? {
            return Ok(());
        }
        progress("selecting", json!({"fallback": true, "k": 0, "n": n}));
        // 2b. careful path: go to each question's tab, reset the cursor to the top (↑ clamps), keys
        for (i, (q, ans)) in qs.iter().zip(&answers).enumerate() {
            progress(if matches!(ans, Answer::Text(_)) { "typing" } else { "selecting" }, json!({"question": q.id, "k": i, "n": n}));
            let (sc, tail) = look()?;
            let Some(cur) = locate(&sc) else {
                return Err(stuck(format!("질문 창에서 현재 질문을 알아보지 못했습니다({i}/{n}개 답함); 보내지 않았습니다"), &sc, &tail));
            };
            if cur != i {
                if !tabs {
                    return Err(stuck(format!("질문 '{}' 이 화면에 없습니다", q.id), &sc, &tail));
                }
                for _ in 0..((i + n + 1 - cur) % (n + 1)) {
                    send("\t", false)?;
                }
                let (sc, tail) = look()?;
                if locate(&sc) != Some(i) {
                    return Err(stuck(format!("질문 '{}' 탭으로 옮기지 못했습니다({i}/{n}개 답함)", q.id), &sc, &tail));
                }
            }
            for _ in 0..q.options.len() + 2 {
                send("\x1b[A", false)?;
            }
            // the cursor must now be on this question's first option: proves which question has
            // the keyboard before anything is toggled or selected
            {
                let (sc, tail) = look()?;
                let first = q.options.first().map(|o| o.label.trim().to_string()).unwrap_or_default();
                let ok = match &sc {
                    Screen::Dialog { lines, .. } => crate::ask::cursor_label(lines).is_some_and(|l| crate::ask::label_matches(&l, &first)),
                    _ => false,
                };
                if !ok {
                    return Err(stuck(
                        format!("'{}' 의 첫 선택지('{first}')에 커서가 오지 않았습니다; 다른 질문이 열려 있을 수 있어 보내지 않았습니다({i}/{n}개 답함)", q.id),
                        &sc,
                        &tail,
                    ));
                }
            }
            match ans {
                Answer::Many(want) => {
                    // idempotent: read each row's checkbox and toggle only rows that differ
                    // (a retry or the user's own clicks may have checked some already)
                    for idx in 0..q.options.len() {
                        let (sc, tail) = look()?;
                        let Screen::Dialog { lines, .. } = &sc else {
                            return Err(stuck(format!("'{}' 선택 중 질문 창이 사라졌습니다", q.id), &sc, &tail));
                        };
                        let Some(row) = lines.iter().find(|l| l.starts_with('\u{f054}')) else {
                            return Err(stuck(format!("'{}' 의 커서 줄을 찾지 못했습니다", q.id), &sc, &tail));
                        };
                        if !crate::ask::cursor_label(lines).is_some_and(|l| crate::ask::label_matches(&l, &q.options[idx].label)) {
                            return Err(stuck(
                                format!("'{}' 의 {}번째 선택지에 커서가 없습니다(화면 갱신 지연 또는 다른 질문); 멈췄습니다", q.id, idx + 1),
                                &sc,
                                &tail,
                            ));
                        }
                        let checked = row.contains('\u{f14a}') || row.contains('\u{f046}');
                        if checked != want.contains(&idx) {
                            send(" ", false)?;
                        }
                        if idx + 1 < q.options.len() {
                            send("\x1b[B", false)?;
                        }
                    }
                    send("", true)?; // Enter on an option row: next question (on "Other" it would open the prompt)
                }
                _ => {
                    let ks = keys(q, 0, ans).map_err(|e| (400, e))?;
                    let text_answer = matches!(ans, Answer::Text(_));
                    let last = ks.len().saturating_sub(1);
                    for (k, (text, enter)) in ks.into_iter().enumerate() {
                        if text_answer && k == last {
                            // Enter on "Other" must have opened the text prompt; it is prefilled
                            // with an earlier custom answer, so clear it (Ctrl+U) before typing
                            let (sc, tail) = look()?;
                            if !matches!(sc, Screen::Prompt { custom: true, .. }) {
                                return Err(stuck(format!("'{}' 의 직접 입력 칸이 열리지 않았습니다", q.id), &sc, &tail));
                            }
                            for _ in 0..4 {
                                send("\u{15}", false)?;
                            }
                        }
                        send(&text, enter)?;
                    }
                    if text_answer && q.multi {
                        // custom text normally leaves a multi-select question on its tab
                        let (sc, _) = look()?;
                        if locate(&sc) == Some(i) {
                            send("\t", false)?;
                        }
                    }
                }
            }
            // 3. moved on? (next question, the review page, or — single question — closed)
            if n > 1 {
                let (sc, tail) = look()?;
                if locate(&sc) != Some(i + 1) {
                    return Err(stuck(
                        format!("'{}' 답을 넣은 뒤 다음 질문으로 넘어가지 않았습니다({}/{n}개 답함); 보내지 않았습니다", q.id, i + 1),
                        &sc,
                        &tail,
                    ));
                }
            }
        }
        if tabs && n > 1 {
            // 4. review page: exactly these answers, then Enter submits
            progress("submitting", json!({"k": n, "n": n}));
            let (sc, tail) = look()?;
            let Screen::Dialog { review: true, lines, .. } = &sc else {
                return Err(stuck("검토(Submit) 화면이 아닙니다; 보내지 않았습니다".into(), &sc, &tail));
            };
            let got = review_answers(lines, qs);
            for (k, (q, ans)) in qs.iter().zip(&answers).enumerate() {
                let want = review_text(q, ans);
                // a cut answer matches by prefix; an answer line that is missing (scrolled off the
                // review page) is left to the toolResult check after submitting
                if got[k].as_deref().is_some_and(|g| !review_matches(g, &want)) {
                    return Err(stuck(format!("검토 화면의 '{}' 답이 다릅니다(기대: {want}); 보내지 않았습니다", q.tab_label()), &sc, &tail));
                }
            }
            send("", true)?;
        }
        Ok(())
    })();
    progress("verifying", json!({"k": n, "n": n}));
    let needle = format!("\"toolCallId\":\"{}\"", ask.call_id);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(if steps.is_ok() { 15 } else { 5 });
    while std::time::Instant::now() < deadline {
        if let Some(line) = std::fs::read_to_string(&session)
            .ok()
            .and_then(|s| s.lines().rev().take(200).find(|l| l.contains(&needle) && l.contains("\"role\":\"toolResult\"")).map(String::from))
        {
            let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
            let m = &v["message"];
            // multi-question results are under details.results[], one question under details
            let details = &m["details"];
            let per: Vec<Value> = match details["results"].as_array() {
                Some(r) => r.clone(),
                None => vec![details.clone()],
            };
            let all_recorded = qs.iter().zip(&answers).enumerate().all(|(i, (q, ans))| {
                let d = per.iter().find(|d| d["id"].as_str() == Some(q.id.as_str())).or(per.get(i));
                d.is_some_and(|d| result_matches(d, q, ans))
            });
            let mut v = json!({"ok": m["isError"] != true && all_recorded, "verified": all_recorded, "name": a.name, "result": m["content"][0]["text"], "details": details});
            if let Err((_, e)) = &steps {
                // a screen check failed but the agent recorded the answers: success
                v["recoveredFrom"] = json!(e);
            }
            return Ok(v);
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    steps?;
    Err((504, "keys sent but no answer recorded within 15 s; check the terminal".into()))
}

pub fn op_interrupt(snap: &Snapshot, key: &str) -> Res<Value> {
    let a = find(&snap.agents, key).ok_or_else(|| format!("unknown agent {key}"))?;
    let h = a.handle.clone().ok_or("agent has no terminal")?;
    let r = term::interrupt(&h, &a.agent_type)?;
    Ok(json!({"ok": true, "name": a.name, "receipt": r}))
}

pub fn op_ask(snap: &Snapshot, from: Option<&str>, to: &str, request: &str, parent: Option<&str>, opts: &threads::ThreadOpts) -> Res<threads::Thread> {
    let target = find_target(snap, to)?;
    let (from, proj, from_handle) = match from {
        Some(f) if !f.is_empty() && f != "human" => {
            let a = find(&snap.agents, f).ok_or_else(|| format!("unknown requester {f}"))?;
            (a.name.clone(), a.project.clone(), a.handle.clone())
        }
        _ => ("human".to_string(), String::new(), None),
    };
    if request.trim().is_empty() {
        return Err("empty request".into());
    }
    let mut t = threads::create(&from, &proj, &target.name, request, parent, opts)?;
    t.to_handle = target.handle.clone();
    t.to_identity = Some(target.identity.clone());
    t.from_identity = from_handle.as_deref().and_then(|h| snap.agents.iter().find(|a| a.handle.as_deref() == Some(h))).map(|a| a.identity.clone());
    t.from_handle = from_handle;
    t.save()?;
    Ok(t)
}

pub fn op_deliver_anyway(id: &str) -> Res<Value> {
    let mut t = threads::load(id).ok_or_else(|| format!("unknown thread {id}"))?;
    t.force = true;
    if t.status == "failed" && t.phase == "request" {
        t.status = "held_possible_draft".into();
    }
    t.event("deliver_anyway", json!({}));
    t.save()?;
    Ok(t.summary())
}

pub const NAME_REQUEST_LINE: &str = "[adash] 대시보드 이름 지정: 사용자가 이 세션이 무슨 일을 하는지 바로 알아볼 수 있는 짧은 이름(한국어 2~12자 권장, 프로젝트명 반복 금지)과 한 줄 역할을 정해 adash iam \"<이름>\" --role \"<역할>\" 을 실행하고, 다른 작업 없이 턴을 끝내세요.";

/// Self-declared name for the agent `key` (resolved from the caller's handle).
pub fn op_iam(coll: &mut Collector, snap: &Snapshot, key: &str, name: &str, role: Option<&str>) -> Res<Value> {
    let name = name.trim();
    validate_name(name)?;
    let a = find(&snap.agents, key).ok_or_else(|| format!("unknown agent {key}"))?;
    if let Some(o) = snap.agents.iter().find(|o| o.identity != a.identity && o.name.to_lowercase() == name.to_lowercase()) {
        let word = a.title.split_whitespace().next().unwrap_or(&a.agent_type);
        let sugg: String = format!("{name} {word}").chars().take(20).collect();
        return Err(format!(
            "name conflict: '{name}' 은(는) 이미 다른 에이전트가 쓰고 있습니다 → {} (project {}, handle {}). 제안: adash iam \"{}\"",
            o.name,
            o.project,
            o.handle.as_deref().unwrap_or("-"),
            sugg.trim_end()
        ));
    }
    let role = role.map(str::trim).filter(|r| !r.is_empty()).map(String::from);
    coll.registry.self_names.insert(a.identity.clone(), SelfName { name: name.to_string(), role: role.clone(), set_at: now_ms() });
    coll.registry.save()?;
    Ok(json!({"ok": true, "old": a.name, "name": name, "role": role, "nameSource": "self"}))
}

/// Queue one name request (sent by the poller through the delivery gate).
pub fn op_request_name(coll: &mut Collector, snap: &Snapshot, key: &str) -> Result<String, (String, String)> {
    let a = find(&snap.agents, key).ok_or_else(|| (key.to_string(), "unknown agent".to_string()))?;
    let reg = &mut coll.registry;
    if reg.overrides.contains_key(&a.identity) || reg.self_names.contains_key(&a.identity) {
        return Err((a.name.clone(), "already named".into()));
    }
    if crate::network::agent_held(a) {
        return Err((a.name.clone(), "network".into()));
    }
    let now = now_ms();
    if let Some(r) = reg.name_requests.get(&a.identity) {
        if now - r.name_requested_at < NAME_REQUEST_COOLDOWN_MS {
            return Err((a.name.clone(), "asked within 24h".into()));
        }
    }
    reg.name_requests.insert(a.identity.clone(), NameRequest { name_requested_at: now, ..Default::default() });
    Ok(a.name.clone())
}

pub fn op_request_names(coll: &mut Collector, snap: &Snapshot, keys: &[String]) -> Res<Value> {
    let mut queued = vec![];
    let mut skipped = vec![];
    for k in keys {
        match op_request_name(coll, snap, k) {
            Ok(n) => queued.push(n),
            Err((n, r)) => skipped.push(json!({"name": n, "reason": r})),
        }
    }
    coll.registry.save()?;
    Ok(json!({"queued": queued, "skipped": skipped}))
}

pub fn ambiguous_names(snap: &Snapshot) -> Vec<String> {
    snap.agents.iter().filter(|a| a.ambiguous).map(|a| a.name.clone()).collect()
}

/// Deliver queued name requests once each target is idle with an empty composer.
pub fn process_name_requests(coll: &mut Collector, snap: &Snapshot) {
    let mut changed = false;
    let pending: Vec<String> = coll.registry.name_requests.iter().filter(|(_, r)| r.sent_at.is_none()).map(|(id, _)| id.clone()).collect();
    for id in pending {
        if coll.registry.self_names.contains_key(&id) || coll.registry.overrides.contains_key(&id) {
            continue;
        }
        let Some(a) = snap.agents.iter().find(|a| a.identity == id) else { continue };
        let note = match threads::gate(a) {
            Gate::Ok => match term::send_line(a.handle.as_deref().unwrap_or(""), NAME_REQUEST_LINE) {
                Ok(_) => {
                    let r = coll.registry.name_requests.get_mut(&id).unwrap();
                    r.sent_at = Some(now_ms());
                    r.prev_since = a.state_since;
                    r.prev_needs_human = a.needs_human;
                    r.last_error = None;
                    eprintln!("adash: name request sent to {}", a.name);
                    changed = true;
                    continue;
                }
                Err(e) => format!("send failed: {e}"),
            },
            Gate::Busy(r) | Gate::Held(r) => r,
        };
        let r = coll.registry.name_requests.get_mut(&id).unwrap();
        if r.last_error.as_deref() != Some(note.as_str()) {
            r.last_error = Some(note);
            changed = true;
        }
    }
    if changed {
        let _ = coll.registry.save();
    }
}

// ---------- server ----------

fn tick(sh: &Shared, notifier: &mut Notifier) {
    let meta = Meta::load();
    let snap = {
        let mut c = sh.coll.plock();
        match c.collect(&meta) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("adash: collect failed: {e}");
                return;
            }
        }
    };
    // drafts and login prompts change states (auth) before anyone looks at them
    let mut snap = snap;
    attach_drafts(sh, &mut snap);
    notifier.observe(&snap);
    sh.history.plock().observe(&snap);
    // concurrency snapshot every 5 minutes
    if snap.at / 300_000 != sh.last_snapshot.load(std::sync::atomic::Ordering::Relaxed) / 300_000 {
        sh.last_snapshot.store(snap.at, std::sync::atomic::Ordering::Relaxed);
        if let Some(db) = sh.analytics.plock().as_ref() {
            if let Err(e) = db.snapshot(&snap) {
                eprintln!("adash: analytics snapshot: {e}");
            }
        }
    }
    process_name_requests(&mut sh.coll.plock(), &snap);
    // one thread at a time under the write lock (re-read inside it), so a slow terminal read
    // (bounded by the tmux call timeout) never blocks every write route for the whole pass
    for id in threads::load_all().into_iter().filter(|t| t.phase != "done" && t.status != "escalated").map(|t| t.id) {
        let _w = sh.write.plock();
        let Some(mut t) = threads::load(&id) else { continue };
        if threads::step(&mut t, &snap) {
            if let Err(e) = t.save() {
                eprintln!("adash: save thread {}: {e}", t.id);
            }
        }
    }
    decisions_from_transcript(sh, &mut snap);
    for a in snap.agents.iter_mut().filter(|a| a.decisions.as_array().is_some_and(|d| !d.is_empty())) {
        let btw_at = a.session_path.as_deref().map(crate::btw::load).and_then(|v| v.iter().map(|e| e.created).max());
        crate::decisions::settle(a, btw_at);
    }
    // bounded caches: parsed transcripts (live agents + a few recent), finished answer jobs
    if snap.at / 60_000 != sh.last_prune.swap(snap.at / 60_000, std::sync::atomic::Ordering::Relaxed) {
        let live: std::collections::HashSet<std::path::PathBuf> = snap.agents.iter().filter_map(|a| a.session_path.clone()).collect();
        sh.transcripts.plock().prune(&live);
        let cutoff = now_ms() - 3_600_000;
        ANSWER_JOBS.plock().retain(|_, v| matches!(v["state"].as_str(), Some("queued" | "running")) || v["startedAt"].as_i64().unwrap_or(0) > cutoff);
    }
    *sh.snap.plock() = snap;
}

/// Agents without a `lastMessage`: "정하실 것" from
/// the last assistant message of their mapped transcript (cached, incremental).
fn decisions_from_transcript(sh: &Shared, snap: &mut Snapshot) {
    let mut tc = sh.transcripts.plock();
    for a in snap.agents.iter_mut().filter(|a| a.last_message.trim().is_empty() && a.session_path.is_some() && a.state != "working") {
        let Ok(v) = tc.transcript(a, None, 40, None, None) else { continue };
        let Some(text) =
            v["messages"].as_array().and_then(|m| m.iter().rev().find(|x| x["role"] == "assistant").and_then(|x| x["text"].as_str().map(String::from)))
        else {
            continue;
        };
        let (d, from) = crate::decisions::for_message(&text);
        a.decisions = d;
        a.decisions_from = from;
    }
}

fn attach_drafts(sh: &Shared, snap: &mut Snapshot) {
    let d = sh.drafts.plock();
    let au = sh.auth.plock();
    let qd = sh.queued.plock();
    for a in snap.agents.iter_mut() {
        a.queued = a.handle.as_ref().and_then(|h| qd.get(h)).cloned();
        a.draft = a.handle.as_ref().and_then(|h| d.get(h)).filter(|v| !v.is_null()).cloned();
        // a login prompt on the screen: the agent waits for the user whatever its transcript says
        if let Some(v) = a.handle.as_ref().and_then(|h| au.get(h)) {
            a.auth = Some(v.clone());
            a.state = if v["kind"] == "trust" { "trust" } else { "auth" }.into();
            a.state_since = v["since"].as_i64().unwrap_or(a.state_since);
            a.needs_human = a.snoozed_until.is_none();
        }
    }
}

/// The harness of an agent at a sign-in screen is signed in elsewhere since `since`: another agent
/// of it works normally since then, or (Antigravity) a CLI log records a sign-in after it.
fn signed_in_elsewhere(s: &Snapshot, a: &crate::collect::Agent, since: i64) -> bool {
    let other =
        s.agents.iter().any(|o| o.agent_type == a.agent_type && o.identity != a.identity && o.handle.is_some() && o.state != "auth" && o.state_since > since);
    other || (a.agent_type == "antigravity" && crate::agy::signed_in_after(since))
}

/// Messages sent while the agent waited at its "trust this folder?" screen: delivered once the
/// folder is trusted (handle → texts).
pub static TRUST_QUEUE: std::sync::LazyLock<Mutex<HashMap<String, Vec<String>>>> = std::sync::LazyLock::new(Default::default);

/// Pick "trust" (or "no") on the trust screen: move to that option, Enter, wait until the screen
/// moved on; then deliver the queued messages. Only when the accept option reads exactly as known.
fn answer_trust(sh: &Shared, h: &str, accept: bool) -> Res<Value> {
    let lk = term::send_lock(h);
    let g = lk.plock();
    let a = sh.snap.plock().agents.iter().find(|a| a.handle.as_deref() == Some(h)).cloned().ok_or("unknown terminal")?;
    let lines = term::screen(h)?;
    let v = crate::auth::trust(&a.agent_type, &lines).ok_or("no trust screen on this terminal")?;
    let target = v[if accept { "yes" } else { "no" }].as_u64().ok_or("the option was not found on the screen")? as usize;
    let (key, n) = crate::auth::moves(&v, target).ok_or("no option is selected on the screen")?;
    for _ in 0..n {
        term::send_keys(h, &[key])?;
        std::thread::sleep(Duration::from_millis(150));
    }
    term::send_keys(h, &["Enter"])?;
    let t0 = now_ms();
    let mut moved = false;
    while now_ms() - t0 < 10_000 {
        std::thread::sleep(Duration::from_millis(400));
        if term::screen(h).map(|l| crate::auth::trust(&a.agent_type, &l).is_none()).unwrap_or(false) {
            moved = true;
            break;
        }
    }
    crate::launch::AUTO_TRUST.plock().remove(h);
    sh.auth.plock().remove(h);
    drop(g);
    if !moved {
        return Err("the screen did not move on after Enter".into());
    }
    let queued = if accept { TRUST_QUEUE.plock().remove(h).unwrap_or_default() } else { vec![] };
    let mut sent = 0;
    if !queued.is_empty() {
        // let the harness come up to its input box
        std::thread::sleep(Duration::from_secs(4));
        refresh(sh);
        for t in &queued {
            let snap = sh.snap.plock().clone();
            if op_send(&snap, &a.name, t, &[], true).is_ok_and(|r| r["delivered"] == true) {
                sent += 1;
            }
        }
    }
    refresh(sh);
    Ok(json!({"ok": true, "accepted": accept, "queuedDelivered": sent, "queued": queued.len()}))
}

/// Login prompts on live agents' screens (every ~10 s; cleared once the screen moves on).
fn scan_auth(sh: &Arc<Shared>) {
    let agents = sh.snap.plock().agents.clone();
    let prev = sh.auth.plock().clone();
    let mut found = HashMap::new();
    let prev_q = sh.queued.plock().clone();
    let mut queued = HashMap::new();
    for a in &agents {
        let Some(h) = &a.handle else { continue };
        let Ok(lines) = term::screen(h) else { continue };
        if let Some(mut v) = crate::auth::queued(&a.agent_type, &lines) {
            v["since"] = prev_q.get(h).and_then(|p| p["since"].as_i64()).map_or(json!(now_ms()), |s| json!(s));
            queued.insert(h.clone(), v);
        }
        if let Some(mut v) = crate::auth::trust(&a.agent_type, &lines) {
            // a terminal the dashboard started in a folder the user chose: accept at once
            if v["yes"].is_u64()
                && crate::config::get().launch.auto_trust
                && (crate::launch::AUTO_TRUST.plock().contains(h) || term::pane_opt(h, "@adash_trust").is_some())
            {
                let (h2, name) = (h.clone(), a.name.clone());
                let sh2 = sh.clone();
                std::thread::spawn(move || {
                    if let Err(e) = answer_trust(&sh2, &h2, true) {
                        eprintln!("adash: auto-trust {name}: {e}");
                    }
                });
            }
            v["kind"] = json!("trust");
            v["cwd"] = json!(a.worktree_path);
            v["queued"] = json!(TRUST_QUEUE.plock().get(h).map_or(0, |q| q.len()));
            v["since"] = prev.get(h).and_then(|p| p["since"].as_i64()).map_or(json!(now_ms()), |s| json!(s));
            found.insert(h.clone(), v);
        } else if let Some(mut v) = crate::auth::detect(&a.agent_type, &lines) {
            v["since"] = prev.get(h).and_then(|p| p["since"].as_i64()).map_or(json!(now_ms()), |s| json!(s));
            found.insert(h.clone(), v);
        }
    }
    *sh.auth.plock() = found;
    *sh.queued.plock() = queued;
}

/// B4 detection (every ~60 s): pointer lines `[marker] @brief 를 읽고 답해줘` in live agents'
/// transcripts that no thread knows yet (exchanges sent before the hook existed, or by hand).
/// The sender is the live agent whose recent tool calls touched the brief, else "unknown".
fn scan_external_threads(sh: &Shared) {
    static POINTER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"^\[([^\]]{2,80})\] @(\S+) 를 읽고 답해줘").unwrap());
    let agents = sh.snap.plock().agents.clone();
    let known: std::collections::HashSet<(String, String)> =
        threads::load_all().into_iter().filter_map(|t| t.marker.clone().map(|m| (m, t.to.clone()))).collect();
    let since = now_ms() - 7 * 86_400_000;
    for a in agents.iter().filter(|a| a.mapping_exact && a.session_path.is_some()) {
        let v = match sh.transcripts.plock().transcript(a, None, 400, None, None) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let msgs = v["messages"].as_array().cloned().unwrap_or_default();
        for (i, m) in msgs.iter().enumerate() {
            if m["role"] != "user" || m["ts"].as_i64().unwrap_or(0) < since {
                continue;
            }
            let Some(c) = POINTER.captures(m["text"].as_str().unwrap_or("")) else { continue };
            let (marker, brief) = (c[1].to_string(), c[2].to_string());
            if known.contains(&(marker.clone(), a.name.clone())) || marker.starts_with("adash ") {
                continue;
            }
            let brief_name = brief.rsplit('/').next().unwrap_or(&brief).to_string();
            let sender = agents
                .iter()
                .filter(|o| o.identity != a.identity)
                .find(|o| sh.transcripts.plock().recent_tool_paths(o, 200).iter().any(|p| p.ends_with(&brief_name)))
                .map(|o| o.name.clone())
                .unwrap_or_else(|| "unknown".into());
            let text = m["text"].as_str().unwrap_or("");
            // peer_review.py's mode suffixes (MODE_SUFFIX)
            let mode = if text.contains("제안자로서") {
                Some("propose")
            } else if text.contains("'수락' 또는 '수정 필요'") {
                Some("accept")
            } else if text.contains("수락 판정이 아니라") {
                Some("discuss")
            } else {
                None
            };
            let kind = if mode == Some("propose") { "propose" } else { "review" };
            let _w = sh.write.plock();
            if let Ok(mut t) = threads::register_external(&sender, &a.name, &marker, &brief, kind, mode, m["ts"].as_i64().unwrap_or(0), "transcript") {
                let answered = msgs[i + 1..].iter().any(|x| x["role"] == "assistant");
                if answered && t.status == "delivered" {
                    t.status = "answered".into();
                    t.phase = "done".into();
                    let _ = t.save();
                }
            }
        }
    }
}

/// One delivery attempt for a queued magi run (same gate as chat). Held → status "held" with the
/// reason, one notification for a draft hold; the server loop retries every ~10 s.
fn deliver_magi(s: &Snapshot, st: &mut crate::magi::RunState) {
    if !matches!(st.status.as_str(), "queued" | "held") {
        return;
    }
    // "그래도 전달" overrides a possible draft only, never a running turn
    let busy = find(&s.agents, &st.source).is_some_and(|a| a.state == "working" || a.state == "asking");
    let net = find(&s.agents, &st.source).is_some_and(crate::network::agent_held);
    let r = if net {
        Ok(json!({"delivered": false, "held": "network"}))
    } else if busy {
        Ok(json!({"delivered": false, "held": "working"}))
    } else {
        op_send(s, &st.source, &st.line, &[], st.force)
    };
    match r {
        Ok(r) if r["delivered"] == true => {
            st.status = "delivered".into();
            st.held_reason = None;
            st.delivered_at = Some(now_ms());
        }
        Ok(r) => {
            let reason = r["held"].as_str().unwrap_or("held").to_string();
            st.status = "held".into();
            if reason.contains("draft") && !st.notified_held {
                st.notified_held = true;
                crate::notify::send_for(
                    &format!("adash 전송 보류: magi → {}", st.source),
                    "입력창에 초안이 있어 magi 지시를 보내지 않았습니다. 대시보드에서 '그래도 전달'하거나 초안을 비우세요.",
                    None,
                    Some(&st.source),
                );
            }
            st.held_reason = Some(reason);
        }
        Err(e) if e.contains("unknown agent") => {
            st.status = "failed".into();
            st.error = Some(e);
        }
        Err(e) => {
            st.status = "held".into();
            st.held_reason = Some(format!("error: {e}"));
        }
    }
    let _ = st.save();
}

/// Composer scan for idle agents (every ~10 s): unsent text in their own input boxes.
fn scan_drafts(sh: &Shared) {
    let agents = sh.snap.plock().agents.clone();
    let mut found = HashMap::new();
    for a in agents.iter().filter(|a| a.state != "working" && a.state != "asking" && matches!(a.agent_type.as_str(), "omp" | "claude" | "codex")) {
        let Some(h) = &a.handle else { continue };
        let Ok((source, tail)) = term::screen(h).map(|t| ("screen".to_string(), t)).or_else(|_| term::tail(h)) else { continue };
        let v = match threads::composer_text(&a.agent_type, &source, &tail) {
            Some(Some(text)) => json!({"text": text, "readable": true, "source": source, "seenAt": now_ms()}),
            Some(None) => Value::Null,
            None => json!({"text": null, "readable": false, "source": source, "seenAt": now_ms()}),
        };
        found.insert(h.clone(), v);
    }
    *sh.drafts.plock() = found;
}

/// `POST /api/agents/{name}/draft {action: submit|clear}` on the agent's own input box.
/// submit = Enter, verified (the box empties; one retry); clear = raw DEL bytes from the end
/// until the box reads empty.
fn op_draft(sh: &Shared, a: &crate::collect::Agent, action: &str) -> Result<Value, (u16, String)> {
    let h = a.handle.clone().ok_or((409, "agent has no terminal".to_string()))?;
    let read = || {
        term::screen(&h)
            .map(|t| ("screen".to_string(), t))
            .or_else(|_| term::tail(&h))
            .ok()
            .and_then(|(src, tail)| threads::composer_text(&a.agent_type, &src, &tail))
    };
    let before = read().ok_or((409, "input box not readable; use the terminal".to_string()))?;
    let Some(text) = before else { return Err((409, "input box is already empty".into())) };
    match action {
        "submit" => {
            if a.state == "working" {
                return Err((409, "agent is working; submitting now would steer it — use the terminal".into()));
            }
            term::send_enter(&h).map_err(|e| (502, e))?;
        }
        "clear" => {
            if a.agent_type != "omp" {
                return Err((409, "clearing is only verified for omp; use the terminal".into()));
            }
            let mut cleared = false;
            // Ctrl+U clears only to the start of the line and `BSpace` is re-encoded under
            // tmux extended-keys, so: raw DEL bytes, one per character plus slack, at the end of
            // the draft; then check, and once more if a little is left
            for round in 0..3 {
                term::send_keys(&h, &["End"]).map_err(|e| (502, e))?;
                let left = match round {
                    0 => text.chars().count(),
                    _ => read().flatten().map(|t| t.chars().count()).unwrap_or(0),
                };
                term::send_dels(&h, left + left / 4 + 20).map_err(|e| (502, e))?;
                std::thread::sleep(Duration::from_millis(400));
                if read() == Some(None) {
                    cleared = true;
                    break;
                }
            }
            if !cleared {
                term::snapshot(&h, "draft-clear-failed");
                return Err((409, "draft did not clear; use the terminal".into()));
            }
        }
        other => return Err((400, format!("unknown action {other} (submit|clear)"))),
    }
    sh.drafts.plock().remove(&h);
    refresh(sh);
    Ok(json!({"ok": true, "name": a.name, "action": action, "text": text}))
}

fn refresh(sh: &Shared) {
    let meta = Meta::load();
    let r = sh.coll.plock().collect(&meta);
    if let Ok(mut s) = r {
        // drafts and screen prompts (auth, trust) are part of every snapshot, also one taken
        // between ticks
        attach_drafts(sh, &mut s);
        *sh.snap.plock() = s;
    }
}

pub fn serve(port: u16) -> Res<()> {
    let server = Server::http(("127.0.0.1", port)).map_err(|e| format!("bind 127.0.0.1:{port}: {e}"))?;
    // the agent server's global environment follows notifications.silenceHarness
    if crate::term::server_pid().is_some() {
        crate::term::harness_env();
    }
    let removed = crate::uploads::cleanup();
    if removed > 0 {
        eprintln!("adash: removed {removed} uploads older than 30 days");
    }
    // state transitions reach the analytics store before history.jsonl's 14-day compaction
    let analytics = match crate::analytics::open() {
        Ok(db) => {
            if let Err(e) = db.ingest_history() {
                eprintln!("adash: analytics history ingest failed: {e}");
            }
            // which ask answers came from the dashboard (chat labels them)
            if let Ok(mut st) = db.conn.prepare("SELECT rec_key FROM asks WHERE channel = 'dashboard'") {
                let ids: Vec<String> = st
                    .query_map([], |r| r.get::<_, String>(0))
                    .map(|it| it.filter_map(Result::ok).filter_map(|k| k.rsplit_once("#ask:").map(|x| x.1.to_string())).collect())
                    .unwrap_or_default();
                crate::ask::DASHBOARD_ANSWERS.plock().extend(ids);
            }
            Some(db)
        }
        Err(e) => {
            eprintln!("adash: analytics store unavailable: {e}");
            None
        }
    };
    let mut coll = Collector::new();
    let first = coll.collect(&Meta::load()).unwrap_or_default();
    // tailnet identity: MagicDNS host for the CSRF origin, owner login(s) allowed through serve
    // (meta.yaml `mobile.allowedLogins` overrides)
    let ts = {
        let (host, owners) = crate::mobile::tailnet();
        let extra: Vec<String> = crate::meta::Meta::load().mobile_allowed;
        (host, if extra.is_empty() { owners } else { extra })
    };
    let sh: Arc<Shared> = Arc::new(Shared {
        snap: Mutex::new(first.clone()),
        coll: Mutex::new(coll),
        write: Mutex::new(()),
        transcripts: Mutex::new(TranscriptCache::default()),
        history: Mutex::new({
            // threads saved before identities: resolve their handles through the session table
            if let Ok(db) = crate::analytics::open_read() {
                for mut t in threads::load_all() {
                    let of = |h: &Option<String>| {
                        h.as_deref().and_then(|h| {
                            db.conn
                                .query_row(
                                    "SELECT file FROM sessions WHERE EXISTS (SELECT 1 FROM json_each(handles) WHERE value = ?1) ORDER BY last_ts DESC LIMIT 1",
                                    [h],
                                    |r| r.get::<_, String>(0),
                                )
                                .ok()
                        })
                    };
                    let (ti, fi) =
                        (t.to_identity.is_none().then(|| of(&t.to_handle)).flatten(), t.from_identity.is_none().then(|| of(&t.from_handle)).flatten());
                    if ti.is_some() || fi.is_some() {
                        t.to_identity = t.to_identity.or(ti);
                        t.from_identity = t.from_identity.or(fi);
                        let _ = t.save();
                    }
                }
            }
            // agents closed from the dashboard before identities were session files: their
            // pane-keyed history and registry entries move to the session file
            let mut h = History::load(now_ms());
            // test sessions recorded before they were filtered: their session files by handle
            if let Ok(db) = crate::analytics::open_read() {
                let handles = crate::history::test_handles();
                let mut files: Vec<String> = vec![];
                if let Ok(mut st) = db.conn.prepare("SELECT file FROM sessions WHERE is_test = 1 OR EXISTS (SELECT 1 FROM json_each(handles) WHERE value = ?1)")
                {
                    for hd in &handles {
                        if let Ok(rows) = st.query_map([hd], |r| r.get::<_, String>(0)) {
                            files.extend(rows.flatten());
                        }
                    }
                }
                files.sort();
                files.dedup();
                h.purge_tests(&files);
            }
            for (from, to) in crate::lifecycle::pane_rekeys() {
                h.rekey(&from, &to);
            }
            let moved = h.rekey_subagents();
            if moved > 0 {
                eprintln!("adash: history: {moved} subagent-file identities moved to their main session");
            }
            h
        }),
        files: Mutex::new(FileIndex::default()),
        analytics: Mutex::new(analytics),
        last_snapshot: std::sync::atomic::AtomicI64::new(0),
        workspaces: Mutex::new(crate::launch::Cached::default()),
        drafts: Mutex::new(HashMap::new()),
        auth: Mutex::new(HashMap::new()),
        queued: Mutex::new(HashMap::new()),
        catalog: Mutex::new(crate::launch::Cached::default()),
        devices: Mutex::new(crate::mobile::Devices::load()),
        ts_host: ts.0,
        ts_allowed: ts.1,
        last_prune: std::sync::atomic::AtomicI64::new(0),
    });
    let _ = SHARED.set(sh.clone());
    {
        // parse session files once so the first /api/history answer is fast
        let sh = sh.clone();
        std::thread::spawn(move || {
            let s = sh.snap.plock().clone();
            let _ = sh.history.plock().query(&s, &Meta::load(), &[], 168.0, now_ms());
        });
    }
    {
        let sh = sh.clone();
        std::thread::spawn(move || {
            let mut n = Notifier::default();
            n.observe(&first);
            if first.at > 0 {
                sh.history.plock().observe(&first);
            }
            loop {
                std::thread::sleep(Duration::from_secs(3));
                guarded("poll", || tick(&sh, &mut n));
            }
        });
    }
    {
        // analytics: incremental ingest of every source each minute (own connection; WAL),
        // daily backup, queued notifications
        let sh = sh.clone();
        std::thread::spawn(move || {
            // retry opening instead of giving up for good
            let db = loop {
                match crate::analytics::open() {
                    Ok(d) => break d,
                    Err(e) => {
                        eprintln!("adash: analytics store: {e}; retrying in 60 s");
                        std::thread::sleep(Duration::from_secs(60));
                    }
                }
            };
            loop {
                guarded("analytics", || analytics_round(&sh, &db));
                std::thread::sleep(Duration::from_secs(60));
            }
        });
    }
    crate::launch::spawn_capacity_poller(&crate::launch::CAPACITY);
    {
        // agents without a session file yet: watch where it will appear (1 s polling, works the
        // same on Linux and macOS) and map it at once instead of on the next 3 s poll
        let sh = sh.clone();
        std::thread::spawn(move || {
            let mut last: HashMap<String, (usize, i64)> = HashMap::new();
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let waiting: Vec<(String, Vec<std::path::PathBuf>)> = sh
                    .snap
                    .plock()
                    .agents
                    .iter()
                    .filter(|a| a.session_path.is_none())
                    .filter_map(|a| {
                        let cwd = a.cwd.clone().unwrap_or_else(|| std::path::PathBuf::from(&a.worktree_path));
                        let dirs = crate::session::new_session_dirs(&a.agent_type, &cwd);
                        (!dirs.is_empty()).then(|| (a.identity.clone(), dirs))
                    })
                    .collect();
                let mut changed = false;
                for (id, dirs) in &waiting {
                    let sig = crate::session::dirs_signature(dirs);
                    if last.insert(id.clone(), sig).is_some_and(|old| old != sig) {
                        changed = true;
                    }
                }
                last.retain(|id, _| waiting.iter().any(|w| &w.0 == id));
                if changed {
                    guarded("session watch", || refresh(&sh));
                }
            }
        });
    }
    {
        // provider reachability: every 30 s, every 10 s while something is unreachable
        let sh = sh.clone();
        std::thread::spawn(move || loop {
            if !crate::config::get().network.enabled {
                return;
            }
            guarded("network", || {
                let ch = crate::network::check();
                on_network_changes(&sh, &ch, false);
            });
            std::thread::sleep(Duration::from_secs(crate::network::interval_s()));
        });
    }
    {
        let sh = sh.clone();
        std::thread::spawn(move || {
            let mut n = 0u32;
            loop {
                std::thread::sleep(Duration::from_secs(10));
                n += 1;
                guarded("background", || background_round(&sh, n));
            }
        });
    }
    eprintln!("adash: serving http://127.0.0.1:{port}");
    for req in server.incoming_requests() {
        let sh = sh.clone();
        std::thread::spawn(move || handle(&sh, req));
    }
    Ok(())
}

/// Run one round of a background loop; a panic is logged and the loop goes on (locks recover
/// from poisoning through `plock`).
fn guarded(name: &str, f: impl FnOnce()) {
    if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        let msg = e.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| e.downcast_ref::<String>().cloned()).unwrap_or_default();
        eprintln!("adash: {name} loop panicked: {msg}; continuing");
    }
}

static REPORT_BUILDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Ingest, notifications, backups; scheduled reports on their own thread (Chromium must not
/// stall ingest).
fn analytics_round(sh: &Arc<Shared>, db: &crate::analytics::Db) {
    let snap = sh.snap.plock().clone();
    let ctx = crate::analytics::context(Some(&snap));
    match crate::analytics::ingest_all(db, &ctx) {
        Ok(r) if r["filesChanged"].as_u64().unwrap_or(0) > 50 => eprintln!("adash: analytics ingest {r}"),
        Ok(_) => {}
        Err(e) => eprintln!("adash: analytics ingest failed: {e}"),
    }
    db.drain_notifications();
    let moves: Vec<(String, String)> = std::mem::take(&mut *crate::history::REKEYS.plock());
    for (from, to) in moves {
        let _ = db.conn.execute("UPDATE OR IGNORE state_transitions SET identity = ?2 WHERE identity = ?1", rusqlite::params![from, to]);
        let _ = db.conn.execute("DELETE FROM state_transitions WHERE identity = ?1", rusqlite::params![from]);
    }
    // provider errors in files read before the errors table existed: once
    let mark = crate::util::state_dir().join("errors_backfilled");
    if !cfg!(test) && !mark.exists() {
        match db.backfill_errors() {
            Ok(n) => {
                eprintln!("adash: errors backfill: {n} rows");
                let _ = std::fs::write(&mark, n.to_string());
            }
            Err(e) => eprintln!("adash: errors backfill: {e}"),
        }
    }
    db.mark_test_sessions(&snap);
    // agent→agent sends for the timeline's interaction kinds (last 30 days)
    if let Ok(mut st) = db.conn.prepare("SELECT ts, from_agent, text, channel, to_agent FROM agent_sends WHERE ts > ?1") {
        let v: Vec<(i64, Option<String>, String, String, Option<String>)> = st
            .query_map([now_ms() - 30 * 86_400_000], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .map(|it| it.filter_map(Result::ok).collect())
            .unwrap_or_default();
        sh.history.plock().set_agent_sends(v);
    }
    if let Err(e) = db.daily_backup() {
        eprintln!("adash: analytics backup failed: {e}");
    }
    // scheduled reports: weekly Mon 09:00 / monthly 1st 09:00, caught up after downtime
    let due = crate::report::due();
    if !due.is_empty() && !REPORT_BUILDING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        std::thread::spawn(move || {
            guarded("report", || {
                let Ok(db) = crate::analytics::open() else { return };
                for (period, label, from, to) in due {
                    match crate::report::build(&db, &period, &label, from, to, true).or_else(|_| crate::report::build(&db, &period, &label, from, to, false)) {
                        Ok(r) => crate::notify::send(
                            if period == "week" { "주간 보고서가 나왔습니다" } else { "월간 보고서가 나왔습니다" },
                            &format!("{} · 대시보드 보고서에서 열기", r.meta["title"].as_str().unwrap_or("")),
                            None,
                        ),
                        Err(e) => eprintln!("adash: report {period}-{label}: {e}"),
                    }
                }
            });
            REPORT_BUILDING.store(false, std::sync::atomic::Ordering::SeqCst);
        });
    }
}

/// Drafts, magi delivery and synthesis prompts, external thread detection (every 10 s).
fn background_round(sh: &Arc<Shared>, n: u32) {
    backend_watchdog();
    let snap = sh.snap.plock().clone();
    crate::restore::tick(&snap);
    scan_drafts(sh);
    scan_auth(sh);
    scan_btw(&snap);
    for mut st in crate::magi::RunState::load_all().into_iter().filter(|r| matches!(r.status.as_str(), "queued" | "held")) {
        // a run nobody delivered within a day is stale
        if now_ms() - st.created_at > 86_400_000 {
            st.status = "failed".into();
            st.error = Some("24시간 동안 전달하지 못했습니다".into());
            let _ = st.save();
            continue;
        }
        let _w = sh.write.plock();
        deliver_magi(&snap, &mut st);
    }
    // answers all in but the source is idle (it ended its turn before its background
    // waits returned): one line telling it to synthesize
    let ts = threads::load_all();
    for mut st in
        crate::magi::RunState::load_all().into_iter().filter(|r| r.status == "delivered" && r.synth_sent_at.is_none() && now_ms() - r.created_at < 86_400_000)
    {
        let Some((latest, paths)) = st.replies(&ts) else { continue };
        let Some(src) = find(&snap.agents, &st.source) else { continue };
        if src.state == "working" || src.state == "asking" || now_ms() - latest < 30_000 || crate::network::agent_held(src) {
            continue;
        }
        if src.state_since > latest {
            // it worked after the last answer arrived and is idle again: it reported
            st.synth_sent_at = Some(src.state_since);
            let _ = st.save();
            continue;
        }
        let line = format!(
            "[adash magi {}] 대상의 답이 모두 도착했습니다: {}. 지시 파일 {} 의 4–5단계대로 종합해 보고하세요(이미 보고했다면 무시).",
            st.id,
            paths.join(", "),
            st.file
        );
        if let Ok(r) = op_send(&snap, &st.source, &line, &[], false) {
            if r["delivered"] == true {
                st.synth_sent_at = Some(now_ms());
                let _ = st.save();
            }
        }
    }
    if n.is_multiple_of(6) {
        scan_external_threads(sh);
    }
}

fn ctype(v: &str) -> Header {
    Header::from_bytes("Content-Type", v).unwrap()
}

fn reply_json(req: Request, code: u16, v: &Value) {
    let _ = req.respond(
        Response::from_string(v.to_string())
            .with_status_code(code)
            .with_header(ctype("application/json; charset=utf-8"))
            .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap()),
    );
}

fn handle(sh: &Shared, mut req: Request) {
    // bytes, not text: `/api/uploads` carries raw images (read one byte past the cap to detect overflow)
    let mut raw_body = Vec::new();
    let _ = req.as_reader().take(crate::uploads::MAX_BYTES as u64 + 1).read_to_end(&mut raw_body);
    let body = String::from_utf8_lossy(&raw_body).into_owned();
    let (url, query) = match req.url().split_once('?') {
        Some((u, q)) => (u.to_string(), q.to_string()),
        None => (req.url().to_string(), String::new()),
    };
    let method = req.method().clone();
    let segs: Vec<String> = url.trim_matches('/').split('/').map(percent_decode).collect();
    let segs: Vec<&str> = segs.iter().map(String::as_str).collect();
    // CSRF: any page in the browser can POST to 127.0.0.1. Mutations need our custom header
    // (forces a CORS preflight we never approve) and, when sent, a same-origin Origin.
    let hdr = |k: &'static str| req.headers().iter().find(|h| h.field.equiv(k)).map(|h| h.value.as_str().to_string());
    // (1) DNS rebinding: only our own Host names, every method (2) proxied without identity
    let port = crate::util::port();
    if !crate::mobile::host_ok(hdr("Host").as_deref(), port, sh.ts_host.as_deref()) {
        reply_json(req, 403, &json!({"error": "forbidden: unexpected Host"}));
        return;
    }
    if crate::mobile::proxied_without_login(
        hdr("Host").as_deref(),
        hdr("X-Forwarded-For").is_some(),
        hdr("Tailscale-User-Login").as_deref(),
        sh.ts_host.as_deref(),
    ) {
        reply_json(req, 403, &json!({"error": "forbidden: proxied request without a tailnet identity"}));
        return;
    }
    // Phase C access: Tailscale-proxied requests need an allowed login and an approved device.
    // The app shell (static files) still loads for a pending device so it can show its code.
    // the tailnet node behind a tailscale serve request (whois, outside the devices lock)
    let node = crate::mobile::node_from(hdr("X-Forwarded-For").as_deref(), hdr("Tailscale-User-Login").as_deref(), &crate::mobile::whois);
    let mut node_cookie = None;
    let access = {
        let mut ds = sh.devices.plock();
        crate::mobile::check(
            &mut ds,
            hdr("Tailscale-User-Login").as_deref(),
            hdr("Cookie").as_deref(),
            hdr("User-Agent").as_deref().unwrap_or(""),
            &sh.ts_allowed,
            node.as_ref(),
        )
    };
    let device = match access {
        crate::mobile::Access::Local => None,
        crate::mobile::Access::Device(id, cookie) => {
            node_cookie = cookie;
            Some(id)
        }
        crate::mobile::Access::Denied(code, v, cookie) => {
            let is_api = segs.first() == Some(&"api");
            if is_api || v["error"] != "device_pending" {
                let mut r = Response::from_string(v.to_string()).with_status_code(code).with_header(ctype("application/json; charset=utf-8"));
                if let Some(c) = cookie {
                    r = r.with_header(Header::from_bytes("Set-Cookie", c.as_bytes()).unwrap());
                }
                let _ = req.respond(r);
                return;
            }
            if let Some(c) = cookie {
                // static shell for a pending device: set the cookie on the first page load
                let path = if url == "/" { "index.html".to_string() } else { url.trim_start_matches('/').to_string() };
                if let Some(f) = WEB.get_file(&path) {
                    let r = Response::from_data(f.contents().to_vec())
                        .with_header(ctype(mime_for(&path).unwrap_or("application/octet-stream")))
                        .with_header(Header::from_bytes("Set-Cookie", c.as_bytes()).unwrap());
                    let _ = req.respond(r);
                    return;
                }
            }
            None
        }
    };
    if let Some(c) = node_cookie {
        // known Tailscale node without a cookie: hand it one and repeat the request with it
        // (307 keeps the method and body)
        let r = Response::from_data(Vec::new())
            .with_status_code(307)
            .with_header(Header::from_bytes("Location", req.url().as_bytes()).unwrap())
            .with_header(Header::from_bytes("Set-Cookie", c.as_bytes()).unwrap())
            .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
        let _ = req.respond(r);
        return;
    }
    if !matches!(method, Method::Get | Method::Head) {
        let port = crate::util::port();
        let ts_origin = sh.ts_host.as_ref().map(|h| format!("https://{h}"));
        let origin_ok = hdr("Origin")
            .is_none_or(|o| o == format!("http://127.0.0.1:{port}") || o == format!("http://localhost:{port}") || ts_origin.as_deref() == Some(o.as_str()));
        if hdr("X-Adash").as_deref() != Some("1") || !origin_ok {
            reply_json(req, 403, &json!({"error": "forbidden: mutating requests need header X-Adash: 1 and a same-origin Origin"}));
            return;
        }
    }
    // phones (approved devices) may not change the login allowlist, and may open only viewable
    // files on the desktop
    let mut body = body;
    if device.is_some() {
        let mut j: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        if matches!((&method, segs.as_slice()), (Method::Put, ["api", "meta"])) && j.is_object() {
            // keep the desktop's allowlist whatever the phone sent
            let cur = json!(Meta::load());
            j["mobileAllowedLogins"] = cur["mobileAllowedLogins"].clone();
            body = j.to_string();
        }
        let denied = match (&method, segs.as_slice()) {
            (Method::Post, ["api", "agents", _, "open"]) => {
                let p = j["path"].as_str().unwrap_or("").to_ascii_lowercase();
                let p = p.split(['#', '?']).next().unwrap_or("").trim_end_matches(|c: char| c.is_ascii_digit() || c == ':' || c == '-');
                !OPEN_FROM_DEVICE.iter().any(|e| p.ends_with(e))
            }
            _ => false,
        };
        if denied {
            reply_json(req, 403, &json!({"error": "not allowed from a phone; use the desktop"}));
            return;
        }
    }
    // device + push routes need the caller's device id
    let jbody = || serde_json::from_str::<Value>(&body).unwrap_or(Value::Null);
    match (&method, segs.as_slice()) {
        (Method::Get, ["api", "reports"]) => return reply_json(req, 200, &crate::report::list()),
        (Method::Get, ["api", "reports", id, ..]) => {
            let pdf = segs.get(3) == Some(&"pdf");
            if segs.len() > 4 || (segs.len() == 4 && !pdf) {
                return reply_json(req, 404, &json!({"error": "not found"}));
            }
            let Some(path) = crate::report::path_of(id, if pdf { "pdf" } else { "html" }) else {
                return reply_json(req, 404, &json!({"error": format!("unknown report {id}{}", if pdf { " (or no PDF)" } else { "" })}));
            };
            let data = std::fs::read(&path).unwrap_or_default();
            let mut r = Response::from_data(data)
                .with_header(ctype(if pdf { "application/pdf" } else { "text/html; charset=utf-8" }))
                .with_header(Header::from_bytes("Cache-Control", "no-cache").unwrap())
                // reports are static: no scripts, nothing external
                .with_header(Header::from_bytes("Content-Security-Policy", "default-src 'none'; style-src 'unsafe-inline'; img-src data:").unwrap());
            if pdf {
                r = r.with_header(Header::from_bytes("Content-Disposition", format!("inline; filename=\"adash-{id}.pdf\"").as_bytes()).unwrap());
            }
            let _ = req.respond(r);
            return;
        }
        (Method::Post, ["api", "reports"]) => {
            let j = jbody();
            let period = j["period"].as_str().unwrap_or("week").to_string();
            let b = match crate::report::bounds(&period, j["from"].as_str(), j["to"].as_str(), j["prev"].as_bool().unwrap_or(false)) {
                Ok(b) => b,
                Err(e) => return reply_json(req, 400, &json!({"error": e})),
            };
            let id = format!("{}-{}", b.0, b.1);
            // built in the background (PDF takes a few seconds); the UI polls the list for the id
            std::thread::spawn(move || {
                let r = crate::analytics::open().and_then(|db| {
                    crate::report::build(&db, &b.0, &b.1, b.2, b.3, true).or_else(|e| {
                        eprintln!("adash: report PDF: {e}; HTML only");
                        crate::report::build(&db, &b.0, &b.1, b.2, b.3, false)
                    })
                });
                if let Err(e) = r {
                    eprintln!("adash: report {}-{}: {e}", b.0, b.1);
                }
            });
            return reply_json(req, 202, &json!({"id": id}));
        }
        (Method::Get, ["api", "devices"]) => return reply_json(req, 200, &sh.devices.plock().listing()),
        (Method::Post, ["api", "devices", id, act @ ("approve" | "revoke")]) => {
            if device.is_some() {
                return reply_json(req, 403, &json!({"error": "devices are approved on the desktop"}));
            }
            let mut ds = sh.devices.plock();
            let Some(d) = ds.devices.iter_mut().find(|d| d.id == *id) else { return reply_json(req, 404, &json!({"error": format!("unknown device {id}")})) };
            let revoked = if *act == "approve" {
                d.approved = true;
                d.revoked = false;
                vec![]
            } else {
                // a phone is one Tailscale node: every entry of that node goes
                crate::mobile::revoke_node(&mut ds, id)
            };
            let _ = ds.save();
            let v = ds.listing();
            drop(ds);
            if *act == "revoke" {
                // its push subscriptions go with it
                let mut subs = crate::push::Subs::load();
                subs.subs.retain(|s| !s.device_id.as_deref().is_some_and(|d| revoked.iter().any(|r| r == d)));
                let _ = subs.save();
            }
            return reply_json(req, 200, &v);
        }
        (Method::Delete, ["api", "devices", id]) => {
            if device.is_some() {
                return reply_json(req, 403, &json!({"error": "devices are managed on the desktop"}));
            }
            let mut ds = sh.devices.plock();
            let n = ds.devices.len();
            ds.devices.retain(|d| d.id != *id);
            if ds.devices.len() == n {
                return reply_json(req, 404, &json!({"error": format!("unknown device {id}")}));
            }
            let _ = ds.save();
            let v = ds.listing();
            drop(ds);
            let mut subs = crate::push::Subs::load();
            subs.subs.retain(|s| s.device_id.as_deref() != Some(id));
            let _ = subs.save();
            return reply_json(req, 200, &v);
        }
        (Method::Get, ["api", "push", "key"]) => {
            return match crate::push::public_key() {
                Ok(k) => reply_json(req, 200, &json!({"publicKey": k, "idleSupported": crate::push::idle_supported()})),
                Err(e) => reply_json(req, 500, &json!({"error": e})),
            };
        }
        (Method::Post, ["api", "push", "subscribe"]) => {
            let j = jbody();
            let mut subs = crate::push::Subs::load();
            return match subs.subscribe(j["subscription"].clone(), Some(j["settings"].clone()).filter(|s| s.is_object()), device.clone()) {
                Ok(()) => reply_json(req, 200, &json!({"ok": true, "count": subs.subs.len()})),
                Err(e) => reply_json(req, 400, &json!({"error": e})),
            };
        }
        (Method::Post, ["api", "push", "unsubscribe"]) => {
            let j = jbody();
            let mut subs = crate::push::Subs::load();
            return match subs.unsubscribe(j["endpoint"].as_str().unwrap_or("")) {
                Ok(removed) => reply_json(req, 200, &json!({"ok": true, "removed": removed})),
                Err(e) => reply_json(req, 500, &json!({"error": e})),
            };
        }
        (Method::Post, ["api", "push", "test"]) => {
            std::thread::spawn(|| crate::push::dispatch("asking", "adash 테스트 알림", "푸시 알림이 동작합니다.", None));
            return reply_json(req, 200, &json!({"ok": true}));
        }
        _ => {}
    }
    match (&method, segs.as_slice()) {
        (Method::Post, ["api", "uploads"]) => {
            let mime = req.headers().iter().find(|h| h.field.equiv("Content-Type")).map(|h| h.value.as_str().to_string()).unwrap_or_default();
            // the browser's file name, percent-encoded UTF-8 (headers are ASCII)
            // or `?name=`; neither: the server names it pasted-<time>.<ext>
            let fname = req
                .headers()
                .iter()
                .find(|h| h.field.equiv("X-Adash-Filename"))
                .map(|h| percent_decode(h.value.as_str()))
                .or_else(|| query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == "name").map(|(_, v)| percent_decode(v)));
            match crate::uploads::save(&raw_body, &mime, fname.as_deref()) {
                Ok(v) => {
                    if let Some(db) = sh.analytics.plock().as_ref() {
                        db.log_action("upload", "", &json!({"bytes": v["bytes"], "mime": v["mime"], "kind": v["kind"]}));
                    }
                    reply_json(req, 201, &v)
                }
                Err((code, e)) => reply_json(req, code, &json!({"error": e})),
            }
        }
        (Method::Get, ["api", "themes", file]) if file.ends_with(".css") => {
            let name = file.trim_end_matches(".css");
            match crate::theme::get(name).filter(|_| crate::theme::valid_name(name)) {
                Some(t) => {
                    let r = Response::from_string(crate::theme::css(&t))
                        .with_header(ctype("text/css; charset=utf-8"))
                        .with_header(Header::from_bytes("Cache-Control", "no-cache").unwrap())
                        .with_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
                    let _ = req.respond(r);
                }
                None => reply_json(req, 404, &json!({"error": format!("unknown theme {name}")})),
            }
        }
        (Method::Get, ["api", "agents", name, "screen"]) => {
            // the terminal view: the rendered screen with colours, and any login prompt on it
            let a = match agent_for(sh, name, None) {
                Ok(a) => a,
                Err(e) => return reply_json(req, e.0, &json!({"error": e.1})),
            };
            let Some(h) = a.handle.clone() else { return reply_json(req, 409, &json!({"error": "this agent has no terminal"})) };
            match term::screen_ansi(&h) {
                Ok(text) => {
                    let plain = term::screen(&h).unwrap_or_default();
                    reply_json(req, 200, &json!({"handle": h, "text": text, "auth": crate::auth::detect(&a.agent_type, &plain)}))
                }
                Err(e) => reply_json(req, 502, &json!({"error": e})),
            }
        }
        (Method::Post, ["api", "agents", name, "send-now"]) => {
            // the harness holds a message until its turn ends: its own "send now" key
            let a = match agent_for(sh, name, None) {
                Ok(a) => a,
                Err(e) => return reply_json(req, e.0, &json!({"error": e.1})),
            };
            let Some(h) = a.handle.clone() else { return reply_json(req, 409, &json!({"error": "this agent has no terminal"})) };
            let Some(q) = term::screen(&h).ok().and_then(|l| crate::auth::queued(&a.agent_type, &l)) else {
                return reply_json(req, 409, &json!({"error": "no held message on the screen"}));
            };
            let key = q["sendNowKey"].as_str().unwrap_or("Enter").to_string();
            match term::send_keys(&h, &[key.as_str()]) {
                Ok(v) => {
                    sh.queued.plock().remove(&h);
                    log_action(sh, &method, &segs, &body);
                    reply_json(req, 200, &v)
                }
                Err(e) => reply_json(req, 502, &json!({"error": e})),
            }
        }
        (Method::Post, ["api", "agents", name, "trust"]) => {
            let a = match agent_for(sh, name, None) {
                Ok(a) => a,
                Err(e) => return reply_json(req, e.0, &json!({"error": e.1})),
            };
            let Some(h) = a.handle.clone() else { return reply_json(req, 409, &json!({"error": "this agent has no terminal"})) };
            let j: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            match answer_trust(sh, &h, j["accept"] != false) {
                Ok(v) => {
                    log_action(sh, &method, &segs, &body);
                    reply_json(req, 200, &v)
                }
                Err(e) => reply_json(req, 409, &json!({"error": e})),
            }
        }
        (Method::Post, ["api", "agents", name, "term-input"]) => {
            // keys typed in the dashboard's terminal view go to the agent's pane as they are
            let a = match agent_for(sh, name, None) {
                Ok(a) => a,
                Err(e) => return reply_json(req, e.0, &json!({"error": e.1})),
            };
            let Some(h) = a.handle.clone() else { return reply_json(req, 409, &json!({"error": "this agent has no terminal"})) };
            let j: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let mut r = Ok(json!({}));
            if let Some(t) = j["text"].as_str().filter(|t| !t.is_empty()) {
                if t.len() > 4096 {
                    return reply_json(req, 413, &json!({"error": "text longer than 4096 bytes"}));
                }
                r = term::send_text(&h, t);
            }
            if let (Ok(_), Some(k)) = (&r, j["key"].as_str()) {
                if !crate::auth::KEYS.contains(&k) {
                    return reply_json(req, 400, &json!({"error": format!("unknown key {k}"), "keys": crate::auth::KEYS}));
                }
                r = term::send_keys(&h, &[k]);
            }
            match r {
                Ok(v) => {
                    log_action(sh, &method, &segs, &body);
                    reply_json(req, 200, &v)
                }
                Err(e) => reply_json(req, 502, &json!({"error": e})),
            }
        }
        (Method::Get, ["api", "agents", name, "attachment"]) => {
            // an image a tool returned (role "media" message): read from the session file on demand
            let q = |k: &str| query.split('&').filter_map(|kv| kv.split_once('=')).find(|(x, _)| *x == k).map(|(_, v)| percent_decode(v));
            let a = match agent_for(sh, name, q("identity").as_deref()) {
                Ok(a) => a,
                Err(e) => return reply_json(req, 404, &json!({"error": e.1})),
            };
            let (Some(path), Some(off), Some(i)) = (
                a.session_path.clone(),
                q("msg").and_then(|m| m.strip_prefix("img").and_then(|o| o.parse::<u64>().ok())),
                q("i").and_then(|i| i.parse::<usize>().ok()),
            ) else {
                return reply_json(req, 400, &json!({"error": "msg=img<offset>&i=<n> and a session file are required"}));
            };
            let etag = format!("\"img-{off}-{i}-{}\"", crate::util::mtime_ms(&path));
            if req.headers().iter().any(|h| h.field.equiv("If-None-Match") && h.value.as_str().split(',').any(|t| t.trim() == etag)) {
                let _ = req.respond(Response::from_data(Vec::new()).with_status_code(304).with_header(Header::from_bytes("ETag", etag.as_bytes()).unwrap()));
                return;
            }
            match crate::transcript::image_at(&a.agent_type, &path, off, i) {
                Ok((mime, bytes)) => {
                    let r = Response::from_data(bytes)
                        .with_header(ctype(&mime))
                        .with_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap())
                        .with_header(Header::from_bytes("Cache-Control", "private, max-age=86400").unwrap())
                        .with_header(Header::from_bytes("ETag", etag.as_bytes()).unwrap());
                    let _ = req.respond(r);
                }
                Err(e) => reply_json(req, 404, &json!({"error": e})),
            }
        }
        (Method::Get, ["api", "agents", name, "file"]) => {
            let ident = query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == "identity").map(|(_, v)| percent_decode(v));
            let a = agent_for(sh, name, ident.as_deref()).ok();
            let raw = query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == "path").map(|(_, v)| percent_decode(v)).unwrap_or_default();
            match a.ok_or((404, format!("unknown agent {name}"))).and_then(|a| with_ctx(sh, &a, |ctx| serve_file(ctx, &raw))) {
                Ok((ct, path, md)) => {
                    // validators: the same path is often rewritten in place (figure regenerated)
                    let mtime = md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).unwrap_or_default();
                    let etag = format!("\"{:x}-{:x}\"", mtime.as_nanos(), md.len());
                    let lm = chrono::DateTime::from_timestamp(mtime.as_secs() as i64, 0).unwrap_or_default().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
                    let hdr = |k: &'static str| req.headers().iter().find(|h| h.field.equiv(k)).map(|h| h.value.as_str().to_string());
                    let fresh = match (hdr("If-None-Match"), hdr("If-Modified-Since")) {
                        (Some(inm), _) => inm.split(',').any(|t| t.trim() == etag || t.trim() == "*"),
                        (None, Some(ims)) => chrono::DateTime::parse_from_rfc2822(&ims).is_ok_and(|d| d.timestamp() >= mtime.as_secs() as i64),
                        _ => false,
                    };
                    let base = |r: Response<std::io::Cursor<Vec<u8>>>| {
                        r.with_header(Header::from_bytes("Cache-Control", "no-cache").unwrap())
                            .with_header(Header::from_bytes("ETag", etag.as_bytes()).unwrap())
                            .with_header(Header::from_bytes("Last-Modified", lm.as_bytes()).unwrap())
                    };
                    if fresh {
                        let _ = req.respond(base(Response::from_data(Vec::new()).with_status_code(304)));
                        return;
                    }
                    match std::fs::read(&path) {
                        Ok(data) => {
                            let mut r = base(Response::from_data(data))
                                .with_header(ctype(ct))
                                .with_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
                            if ct.starts_with("image/svg") {
                                r = r.with_header(Header::from_bytes("Content-Security-Policy", "sandbox").unwrap());
                            }
                            let _ = req.respond(r);
                        }
                        Err(e) => reply_json(req, 500, &json!({"error": e.to_string()})),
                    }
                }
                Err((code, e)) => reply_json(req, code, &json!({"error": e})),
            }
        }
        (Method::Get, [first, ..]) if *first != "api" => {
            let path = if url.trim_matches('/').is_empty() { "index.html".to_string() } else { segs.join("/") };
            match WEB.get_file(&path) {
                Some(f) => {
                    // revalidate every time: a new build's CSS/JS must replace the cached copy
                    // (ETag = content hash; unchanged files answer 304)
                    let etag = {
                        use std::hash::{Hash, Hasher};
                        let mut h = std::collections::hash_map::DefaultHasher::new();
                        f.contents().hash(&mut h);
                        format!("\"{:016x}\"", h.finish())
                    };
                    if hdr("If-None-Match").is_some_and(|v| v.split(',').any(|t| t.trim() == etag)) {
                        let r = Response::from_data(Vec::new())
                            .with_status_code(304)
                            .with_header(Header::from_bytes("ETag", etag.as_bytes()).unwrap())
                            .with_header(Header::from_bytes("Cache-Control", "no-cache").unwrap());
                        let _ = req.respond(r);
                        return;
                    }
                    // defence in depth: scripts only from here, no plugins, no framing, no sniffing
                    let r = Response::from_data(f.contents())
                        .with_header(Header::from_bytes("ETag", etag.as_bytes()).unwrap())
                        .with_header(Header::from_bytes("Cache-Control", "no-cache").unwrap())
                        .with_header(ctype(mime_for(&path).unwrap_or("application/octet-stream")))
                        .with_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap())
                        .with_header(
                            Header::from_bytes("Content-Security-Policy", "script-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'self'")
                                .unwrap(),
                        );
                    let _ = req.respond(r);
                }
                None => reply_json(req, 404, &json!({"error": format!("no file /{path}")})),
            }
        }
        _ => {
            let r = api(sh, &method, &segs, &body, &query);
            if r.as_ref().is_ok_and(|(c, _)| *c < 400) {
                log_action(sh, &method, &segs, &body);
            }
            // ?lang=en: English for the labels the server writes (agent message bodies stay as written)
            let en = query.split('&').any(|kv| kv == "lang=en");
            match r {
                Ok((code, mut v)) => {
                    if en {
                        crate::i18n::english(&segs, &mut v);
                    }
                    reply_json(req, code, &v)
                }
                Err((code, e)) => reply_json(req, code, &json!({"error": if en { crate::i18n::error_en(&e) } else { e }})),
            }
        }
    }
}

type ApiRes = Result<(u16, Value), (u16, String)>;

fn bad(e: String) -> (u16, String) {
    if e.starts_with("unknown") {
        (404, e)
    } else if e.starts_with("not delivered") || e.starts_with("held") || e.contains("in use") || e.starts_with("name conflict") {
        (409, e)
    } else {
        (400, e)
    }
}

fn api(sh: &Shared, method: &Method, segs: &[&str], body: &str, query: &str) -> ApiRes {
    let j: Value = if body.trim().is_empty() { json!({}) } else { serde_json::from_str(body).map_err(|e| (400, format!("bad JSON: {e}")))? };
    let snap = || sh.snap.plock().clone();
    let q = |k: &str| query.split('&').filter_map(|kv| kv.split_once('=')).find(|(a, _)| *a == k).map(|(_, v)| percent_decode(v));
    match (method, segs) {
        (Method::Get, ["api", "agents", name, "commands"]) => {
            let a = find(&sh.snap.plock().agents, name).cloned().ok_or((404, format!("unknown agent {name}")))?;
            Ok((200, crate::commands::catalog(&a)))
        }
        (Method::Get, ["api", "agents", name, "transcript"]) => {
            let a = agent_for(sh, name, q("identity").as_deref())?;
            let limit = q("limit").and_then(|l| l.parse().ok()).unwrap_or(200);
            let after = q("after").filter(|s| !s.is_empty());
            let want = q("source");
            let around = q("around");
            sh.transcripts.plock().transcript(&a, after.as_deref(), limit, want.as_deref(), around.as_deref()).map(|v| (200, v)).map_err(|e| (500, e))
        }
        (Method::Get, ["api", "history"]) => {
            let hours = q("hours").and_then(|h| h.parse::<f64>().ok()).unwrap_or(24.0).clamp(1.0, 168.0);
            let s = snap();
            let meta = Meta::load();
            let threads = threads::load_all();
            Ok((200, sh.history.plock().query(&s, &meta, &threads, hours, crate::util::now_ms())))
        }
        (Method::Get, ["api", "workspaces"]) => {
            let s = snap();
            sh.workspaces.plock().get(Duration::from_secs(10), || crate::launch::workspaces(&s)).map(|v| (200, v)).map_err(|e| (502, e))
        }
        (Method::Get, ["api", "launch", "catalog"]) => {
            sh.catalog.plock().get(Duration::from_secs(600), || Ok(crate::launch::catalog())).map(|v| (200, v)).map_err(|e| (500, e))
        }
        (Method::Get, ["api", "capacity"]) => {
            let force = q("refresh").is_some_and(|r| r == "1" || r == "true");
            let mut cap = crate::launch::CAPACITY.plock();
            if force {
                if let Some(v) = cap.refresh(true) {
                    crate::launch::persist_capacity(&v);
                }
            }
            Ok((200, cap.view()))
        }
        (Method::Post, ["api", "sessions"]) => crate::launch::new_session(&j).map(|l| (201, after_launch(sh, &l, &j))),
        (Method::Post, ["api", "worktrees"]) => crate::launch::new_worktree(&j).map(|l| (201, after_launch(sh, &l, &j))),
        (Method::Get, ["api", "themes"]) => {
            let (version, ts) = crate::theme::themes(false);
            let refused: Vec<Value> = crate::theme::load_all_with_refused().1.into_iter().map(|(p, r)| json!({"path": p, "reason": r})).collect();
            let tokens: Vec<Value> = crate::theme::TOKENS
                .iter()
                .map(|(g, k, kind, d)| json!({"group": g, "key": k, "kind": kind, "var": crate::theme::css_var(g, k), "description": d}))
                .collect();
            Ok((
                200,
                json!({"version": version, "dir": crate::theme::user_dir(), "themes": ts.iter().map(crate::theme::view).collect::<Vec<_>>(), "refused": refused, "tokens": tokens, "defaults": {"dark": crate::config::get().theme.dark(), "light": crate::config::get().theme.light()}}),
            ))
        }
        (Method::Post, ["api", "themes", "reload"]) => {
            let (version, ts) = crate::theme::themes(true);
            Ok((200, json!({"version": version, "themes": ts.iter().map(crate::theme::view).collect::<Vec<_>>()})))
        }
        (Method::Post, ["api", "projects", name, action @ ("hide" | "unhide" | "close" | "archive" | "restore" | "reopen-all")]) => {
            let r = op_project(sh, name, action, &j);
            refresh(sh);
            Ok(match r {
                Ok(v) => (200, v),
                Err((c, v)) => (c, v),
            })
        }
        (Method::Get, ["api", "fs", "dirs"]) => {
            let q = query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == "q").map(|(_, v)| percent_decode(v)).unwrap_or_default();
            let mut v = crate::paths::dirs(&q, &crate::util::home());
            if q.trim().is_empty() {
                // empty field: frequent project parents, not a listing of $HOME
                let sug = json!(crate::paths::project_parents(&sh.snap.plock().agents));
                v["items"] = sug.clone();
                v["suggest"] = sug;
            }
            Ok((200, v))
        }
        (Method::Post, ["api", "projects"]) => {
            let (mut body, launched) = crate::launch::new_project(&j)?;
            if let Some(l) = launched {
                body["launch"] = after_launch(sh, &l, &j);
            } else {
                sh.workspaces.plock().invalidate();
            }
            Ok((201, body))
        }
        (Method::Get, ["api", "search"]) => {
            let qs = q("q").unwrap_or_default();
            let (from, to) = (q("from").and_then(|x| x.parse().ok()).unwrap_or(0), q("to").and_then(|x| x.parse().ok()).unwrap_or(i64::MAX));
            let (project, agent) = (q("project"), q("agent"));
            let sq = crate::stats::SearchQuery {
                q: &qs,
                project: project.as_deref(),
                agent: agent.as_deref(),
                from,
                to,
                limit: q("limit").and_then(|l| l.parse().ok()).unwrap_or(50),
            };
            let mut hits = {
                let db = crate::analytics::open_read().map_err(|e| (503, format!("analytics store unavailable: {e}")))?;
                crate::stats::search(&db, &sq).map_err(|e| (400, e))?
            };
            // live agents: open the hit in their chat
            let s = sh.snap.plock();
            for h in hits.iter_mut() {
                if let Some(a) = s.agents.iter().find(|a| a.session_path.as_ref().is_some_and(|p| p.to_string_lossy() == h["file"].as_str().unwrap_or(""))) {
                    h["agent"] = json!(a.name);
                    h["live"] = json!(true);
                }
            }
            Ok((200, json!({"q": qs, "hits": hits})))
        }
        // past sessions by title / adash name / project / first prompt (resume with POST
        // /api/agents/{name}/reopen {identity: file})
        (Method::Get, ["api", "sessions", "search"]) => {
            let qs = q("q").unwrap_or_default();
            let mut hits = {
                let db = crate::analytics::open_read().map_err(|e| (503, format!("analytics store unavailable: {e}")))?;
                crate::stats::search_sessions(&db, &qs, q("limit").and_then(|l| l.parse().ok()).unwrap_or(30)).map_err(|e| (400, e))?
            };
            let s = sh.snap.plock();
            for h in hits.iter_mut() {
                if let Some(a) = s.agents.iter().find(|a| a.session_path.as_ref().is_some_and(|p| p.to_string_lossy() == h["file"].as_str().unwrap_or(""))) {
                    h["agent"] = json!(a.name);
                    h["live"] = json!(true);
                }
            }
            Ok((200, json!({"q": qs, "sessions": hits})))
        }
        (Method::Get, ["api", "sessions", id, "transcript"]) => {
            let sid: i64 = id.parse().map_err(|_| (400, "session id must be a number".to_string()))?;
            let (file, kind): (String, String) = {
                let db = crate::analytics::open_read().map_err(|e| (503, format!("analytics store unavailable: {e}")))?;
                db.conn
                    .query_row("SELECT file, agent_type FROM sessions WHERE id = ?1", [sid], |r| Ok((r.get(0)?, r.get(1)?)))
                    .map_err(|_| (404, format!("unknown session {sid}")))?
            };
            let limit = q("limit").and_then(|l| l.parse().ok()).unwrap_or(200);
            let around = q("around");
            let mut v = sh.transcripts.plock().session_view(&kind, std::path::Path::new(&file), around.as_deref(), limit).map_err(|e| (500, e))?;
            v["session"] = json!(sid);
            Ok((200, v))
        }
        (Method::Get, ["api", "stats"]) => {
            let by = q("by").unwrap_or_else(|| "all".into());
            let (from, to) = crate::stats::bounds(q("from").as_deref(), q("to").as_deref()).map_err(|e| (400, e))?;
            let db = crate::analytics::open_read().map_err(|e| (503, format!("analytics store unavailable: {e}")))?;
            if by == "pair" {
                return crate::stats::pairs(&db, from, to).map(|v| (200, v)).map_err(|e| (400, e));
            }
            if by == "errors" {
                return crate::stats::errors(&db, from, to).map(|v| (200, v)).map_err(|e| (400, e));
            }
            crate::stats::stats(&db, from, to, &by).map(|v| (200, v)).map_err(|e| (400, e))
        }
        (Method::Get, ["api", "state"]) => {
            let meta = Meta::load();
            let ts = threads::load_all();
            let mut v = state::build(&snap(), &meta, &ts);
            v["restoreHeld"] = json!(crate::restore::held());
            let cutoff = now_ms() - 7 * 86_400_000;
            {
                let now = now_ms();
                let snap = snap();
                v["hiddenProjects"] = json!(meta
                    .projects
                    .iter()
                    .filter(|(_, p)| p.is_hidden(now) || p.archived)
                    .map(|(n, p)| json!({"name": n, "until": p.hidden.as_ref().and_then(|h| h.until), "archived": p.archived, "agents": snap.agents.iter().filter(|a| &a.project == n).count(), "archivedSessions": p.archived_sessions}))
                    .collect::<Vec<_>>());
            }
            v["magiRuns"] = json!(crate::magi::RunState::load_all().iter().filter(|r| r.created_at > cutoff).take(30).map(|r| r.view(&ts)).collect::<Vec<_>>());
            v["backend"] = json!({"kind": "tmux", "available": term::status()});
            v["network"] = crate::network::view();
            // changes when a theme file is saved: the UI reloads its stylesheet (live preview)
            v["themeVersion"] = json!(crate::theme::themes(false).0);
            v["features"] =
                json!({"magi": crate::magi::enabled(), "network": crate::config::get().network.enabled, "phones": crate::config::get().mobile.tailscale});
            if let Some(agents) = v["agents"].as_array_mut() {
                for a in agents.iter_mut() {
                    let p = crate::network::provider_of(a["agentType"].as_str().unwrap_or(""), a["model"].as_str());
                    a["provider"] = json!(p);
                    a["networkHeld"] = json!(crate::network::held(a["agentType"].as_str().unwrap_or(""), a["model"].as_str()));
                }
            }
            // per-session usage (incremental reads; account limits cached)
            {
                let snap = snap();
                if let Some(agents) = v["agents"].as_array_mut() {
                    for j in agents.iter_mut() {
                        if let Some(a) = snap.agents.iter().find(|a| j["identity"].as_str() == Some(a.identity.as_str())) {
                            j["usage"] = crate::usage::view(a);
                        }
                    }
                }
            }
            {
                let open = BTW_OPEN.plock();
                for a in v["agents"].as_array_mut().into_iter().flatten() {
                    if a["agentType"] == "omp" {
                        a["btw"] = a["handle"].as_str().and_then(|h| open.get(h)).cloned().unwrap_or(json!({"open": false}));
                    }
                }
            }
            Ok((200, v))
        }
        (Method::Get, ["api", "agents", name, "paths"]) => {
            let a = find(&sh.snap.plock().agents, name).cloned().ok_or((404, format!("unknown agent {name}")))?;
            let t0 = std::time::Instant::now();
            let wt = std::path::PathBuf::from(&a.worktree_path);
            let base = crate::session::proc_cwd(&a).unwrap_or_else(|| wt.clone());
            let mut items = crate::paths::complete(
                &sh.files,
                &wt,
                &base,
                &q("q").unwrap_or_default(),
                q("limit").and_then(|l| l.parse().ok()).unwrap_or(50),
                q("all").as_deref() == Some("1"),
            )?;
            for c in items.iter_mut() {
                c.insert = Some(crate::paths::mention(&a.agent_type, &c.path));
            }
            Ok((200, json!({"base": base, "items": items, "format": crate::paths::mention_format(&a.agent_type), "ms": t0.elapsed().as_millis() as u64})))
        }
        (Method::Post, ["api", "agents", name, "open" | "stat"]) => {
            let a = agent_for(sh, name, q("identity").as_deref())?;
            with_ctx(sh, &a, |ctx| {
                let bases = bases_of(&j["base"]);
                if segs[3] == "open" {
                    let raw = with_base(ctx, j["path"].as_str().unwrap_or(""), &bases);
                    op_open(ctx, &raw).map(|v| (200, v))
                } else {
                    op_stat(ctx, j["paths"].as_array().map(Vec::as_slice).unwrap_or(&[]), &bases).map(|v| (200, v))
                }
            })
        }
        (Method::Post, ["api", "agents", name, "close"]) => {
            let s = snap();
            let a = find_target(&s, name).map_err(|e| (404, e))?.clone();
            let r = crate::lifecycle::close(&s, &a, &j);
            refresh(sh);
            Ok(match r {
                Ok(v) => (200, v),
                Err((c, v)) => (c, v),
            })
        }
        (Method::Post, ["api", "agents", name, "restart"]) => {
            let s = snap();
            let a = find_target(&s, name).map_err(|e| (404, e))?.clone();
            let r = crate::lifecycle::restart(&s, &a, &j);
            refresh(sh);
            Ok(match r {
                Ok(v) => (200, v),
                Err((c, v)) => (c, v),
            })
        }
        (Method::Post, ["api", "agents", name, "reopen"]) => {
            let s = snap();
            let r = crate::lifecycle::reopen(
                &s,
                q("identity").or_else(|| j["identity"].as_str().map(String::from)).as_deref().filter(|i| !i.is_empty()).unwrap_or(name),
            );
            Ok(match r {
                Ok(v) => (200, v),
                Err((c, v)) => (c, v),
            })
        }
        (Method::Post, ["api", "close"]) => {
            let mode = j["mode"].as_str().unwrap_or("graceful");
            let mut results = vec![];
            for t in j["targets"].as_array().into_iter().flatten().filter_map(|x| x.as_str()) {
                let s = snap();
                let body = json!({"mode": mode, "confirmWorking": j["confirmWorking"], "discardDraft": j["discardDraft"]});
                let r = match find_target(&s, t) {
                    Ok(a) => {
                        let a = a.clone();
                        crate::lifecycle::close(&s, &a, &body)
                    }
                    Err(e) => Err((404, json!({"error": e, "reason": "unknown"}))),
                };
                refresh(sh);
                results.push(match r {
                    Ok(v) => v,
                    Err((code, mut v)) => {
                        v["name"] = json!(t);
                        v["ok"] = json!(false);
                        v["code"] = json!(code);
                        v
                    }
                });
            }
            Ok((200, json!({"results": results})))
        }
        (Method::Get, ["api", "agents", name, "usage"]) => {
            let a = agent_for(sh, name, q("identity").as_deref())?;
            Ok((200, crate::usage::view(&a)))
        }
        (Method::Get, ["api", "agents", name, "models"]) => {
            let a = agent_for(sh, name, q("identity").as_deref())?;
            Ok((200, crate::model_switch::view(&a)))
        }
        (Method::Post, ["api", "agents", name, "model"]) => {
            let a = agent_for(sh, name, q("identity").as_deref())?;
            let r = crate::model_switch::apply(&a, j["model"].as_str().filter(|s| !s.is_empty()), j["effort"].as_str().filter(|s| !s.is_empty()));
            if let Some(db) = sh.analytics.plock().as_ref() {
                db.log_action("model", &a.name, &json!({"model": j["model"], "effort": j["effort"], "ok": r.is_ok()}));
            }
            refresh(sh);
            r.map(|v| (200, v))
        }
        (Method::Get, ["api", "agents", name, "usage", "hourly"]) => {
            let a = agent_for(sh, name, q("identity").as_deref())?;
            let hours = q("hours").and_then(|h| h.parse().ok()).unwrap_or(48);
            let path = a.session_path.clone().ok_or((404, "no session file mapped to this agent yet".to_string()))?;
            Ok((200, crate::usage_hourly::view(&a.agent_type, &path, hours)))
        }
        (Method::Post, ["api", "agents", name, "btw"]) => {
            let a = find(&sh.snap.plock().agents, name).cloned().ok_or((404, format!("unknown agent {name}")))?;
            op_btw(&a, j["text"].as_str().unwrap_or("")).map(|v| (200, v))
        }
        (Method::Post, ["api", "agents", name, "btw", "close"]) => {
            let a = find(&sh.snap.plock().agents, name).cloned().ok_or((404, format!("unknown agent {name}")))?;
            op_btw_close(&a).map(|v| (200, v))
        }
        (Method::Post, ["api", "agents", name, "retry"]) => {
            let a = find(&sh.snap.plock().agents, name).cloned().ok_or((404, format!("unknown agent {name}")))?;
            op_retry(sh, &a).map(|v| (200, v))
        }
        (Method::Post, ["api", "fix-all"]) => {
            // errors: retry; sign-in screens: restart on the same session when the harness is
            // signed in elsewhere, else say so
            let s = snap();
            let targets: Vec<crate::collect::Agent> = s
                .agents
                .iter()
                .filter(|a| matches!(a.state.as_str(), "error" | "auth") && a.snoozed_until.is_none())
                // optional `targets`: only these agents
                .filter(|a| j["targets"].as_array().is_none_or(|t| t.iter().any(|x| x.as_str() == Some(a.name.as_str()))))
                .cloned()
                .collect();
            let results: Vec<Value> = std::thread::scope(|sc| {
                let hs: Vec<_> = targets
                    .iter()
                    .map(|a| {
                        let (s, j) = (&s, &j);
                        sc.spawn(move || {
                            if a.state == "error" {
                                return match op_retry(sh, a) {
                                    Ok(mut v) => {
                                        v["action"] = json!("retry");
                                        v
                                    }
                                    Err((code, e)) => json!({"name": a.name, "action": "retry", "ok": false, "code": code, "reason": e}),
                                };
                            }
                            // the agent's process started before the sign-in it lacks: compare with its start
                            // (the prompt's first sighting moves when adash restarts)
                            let started = a.handle.as_ref().and_then(|h| crate::session::scan_procs().remove(h)).and_then(|v| v.into_iter().find(|p| p.kind == a.agent_type)).map(|p| p.start_ms);
                            let since = started.unwrap_or_else(|| a.auth.as_ref().and_then(|v| v["since"].as_i64()).unwrap_or(a.state_since));
                            if !signed_in_elsewhere(s, a, since) {
                                return json!({"name": a.name, "action": "none", "ok": false, "reason": "not_signed_in"});
                            }
                            match crate::lifecycle::restart(s, a, j) {
                                Ok(_) => json!({"name": a.name, "action": "restart", "ok": true}),
                                Err((code, v)) => json!({"name": a.name, "action": "restart", "ok": false, "code": code, "reason": v["reason"].as_str().or(v["error"].as_str()).unwrap_or("failed")}),
                            }
                        })
                    })
                    .collect();
                hs.into_iter().filter_map(|h| h.join().ok()).collect()
            });
            refresh(sh);
            Ok((200, json!({"results": results})))
        }
        (Method::Post, ["api", "retry-errors"]) => {
            let errs: Vec<crate::collect::Agent> = sh.snap.plock().agents.iter().filter(|a| a.state == "error" && a.snoozed_until.is_none()).cloned().collect();
            // in parallel (each waits up to 20 s); sends stay serialized per terminal
            let results: Vec<Value> = std::thread::scope(|sc| {
                let hs: Vec<_> = errs
                    .iter()
                    .map(|a| {
                        sc.spawn(move || match op_retry(sh, a) {
                            Ok(v) => v,
                            Err((code, e)) => {
                                json!({"name": a.name, "resumed": false, "state": a.state, "code": code, "error": {"status": null, "message": e}})
                            }
                        })
                    })
                    .collect();
                hs.into_iter().filter_map(|h| h.join().ok()).collect()
            });
            Ok((200, json!({"results": results})))
        }
        (Method::Post, ["api", "agents", name, "draft"]) => {
            let a = find(&sh.snap.plock().agents, name).cloned().ok_or((404, format!("unknown agent {name}")))?;
            op_draft(sh, &a, j["action"].as_str().unwrap_or("")).map(|v| (200, v))
        }
        (Method::Post | Method::Delete, ["api", "agents", name, "ack"]) => {
            let s = snap();
            let a = find(&s.agents, name).ok_or((404, format!("unknown agent {name}")))?;
            let at = (*method == Method::Post).then(now_ms);
            {
                let mut c = sh.coll.plock();
                match at {
                    Some(t) => c.registry.acked.insert(a.identity.clone(), t),
                    None => c.registry.acked.remove(&a.identity),
                };
                c.registry.save().map_err(|e| (500, e))?;
            }
            refresh(sh);
            Ok((200, json!({"ok": true, "name": a.name, "ackedAt": at})))
        }
        (Method::Post, ["api", "agents", name, "answer"]) => {
            // driving the dialog takes tens of seconds: 202 {jobId}, progress at /api/answers/{jobId}
            let a = find(&sh.snap.plock().agents, name).cloned().ok_or((404, format!("unknown agent {name}")))?;
            if a.ask.is_none() {
                return Err((409, "no open question".into()));
            }
            if !crate::harness::get(&a.agent_type).is_some_and(|h| h.answers_questions()) {
                return Err((501, format!("{} questions are answered in the terminal", a.agent_type)));
            }
            if ANSWER_JOBS.plock().values().any(|v| v["agent"] == a.name.as_str() && matches!(v["state"].as_str(), Some("queued" | "running"))) {
                return Err((409, "an answer for this agent is already being entered".into()));
            }
            let id = format!("A{}-{:04x}", now_ms(), std::process::id() as u16 ^ (now_ms() as u16));
            let set = |id: &str, v: Value| {
                ANSWER_JOBS.plock().insert(id.to_string(), v);
            };
            set(
                &id,
                json!({"jobId": id, "agent": a.name, "state": "queued", "step": "queued", "k": 0, "n": a.ask.as_ref().map(|q| q.questions.len()).unwrap_or(1), "verified": null, "startedAt": now_ms()}),
            );
            let Some(sh) = SHARED.get().cloned() else { return Err((500, "server not ready".into())) };
            let job = id.clone();
            let n_q = a.ask.as_ref().map(|q| q.questions.len()).unwrap_or(1);
            std::thread::spawn(move || {
                let t0 = now_ms();
                let progress = |stage: &str, detail: Value| {
                    if let Some(v) = ANSWER_JOBS.plock().get_mut(&job) {
                        v["state"] = json!("running");
                        v["step"] = json!(stage);
                        if let Some(k) = detail.get("k") {
                            v["k"] = k.clone();
                        }
                        if let Some(n) = detail.get("n") {
                            v["n"] = n.clone();
                        }
                        v["detail"] = detail;
                    }
                };
                // mark before the keys go in: the chat may parse the answer before we return
                let call = a.ask.as_ref().map(|q| q.call_id.clone()).unwrap_or_default();
                crate::ask::DASHBOARD_ANSWERS.plock().insert(call.clone());
                let lk = term::send_lock(a.handle.as_deref().unwrap_or(""));
                let r = {
                    let _g = lk.plock();
                    op_answer(&a, &j, &progress)
                };
                if r.is_err() {
                    crate::ask::DASHBOARD_ANSWERS.plock().remove(&call);
                }
                let ms = now_ms() - t0;
                let out = match &r {
                    Ok(v) => {
                        json!({"jobId": job, "agent": a.name, "state": "done", "step": "done", "k": n_q, "n": n_q, "ok": v["ok"], "verified": v["verified"], "result": v, "ms": ms})
                    }
                    Err((code, e)) => {
                        let k = ANSWER_JOBS.plock().get(&job).and_then(|v| v["k"].as_u64()).unwrap_or(0);
                        json!({"jobId": job, "agent": a.name, "state": "failed", "step": "failed", "k": k, "n": n_q, "verified": null, "status": code, "reason": e, "error": e, "ms": ms})
                    }
                };
                eprintln!(
                    "adash: answer {} {job}: {} in {ms} ms{}",
                    a.name,
                    out["state"].as_str().unwrap_or(""),
                    r.as_ref().err().map(|(c, e)| format!(" ({c}: {})", crate::util::truncate(e.lines().next().unwrap_or(""), 160))).unwrap_or_default()
                );
                if let Some(db) = sh.analytics.plock().as_ref() {
                    db.log_action("answer_result", &a.name, &json!({"jobId": job, "state": out["state"], "verified": out["verified"], "status": out["status"], "error": out["reason"], "ms": ms, "recoveredFrom": r.as_ref().ok().and_then(|v| v.get("recoveredFrom").cloned())}));
                    if r.is_ok() {
                        if let (Some(q), Some(p)) = (&a.ask, &a.session_path) {
                            let text = j["text"].as_str().map(String::from).unwrap_or_else(|| j.to_string());
                            db.record_answer(&p.to_string_lossy(), &q.call_id, now_ms(), &text);
                        }
                    }
                }
                let mut out = out;
                out["startedAt"] = json!(t0);
                ANSWER_JOBS.plock().insert(job.clone(), out);
                refresh(&sh);
            });
            Ok((202, json!({"jobId": id, "state": "queued"})))
        }
        (Method::Post, ["api", "network", "check"]) => {
            // test only: {"testUnroutable": "openai"} points that probe at an unroutable address
            // (no notifications, no retries); {"testUnroutable": ""} clears it
            if let Some(t) = j.get("testUnroutable") {
                let mut n = crate::network::NET.plock();
                n.test_override.clear();
                if let Some(p) = t.as_str().filter(|p| !p.is_empty()) {
                    n.test_override.insert(p.to_string(), "https://10.255.255.1/".into());
                }
            }
            let ch = crate::network::check();
            on_network_changes(sh, &ch, j.get("testUnroutable").is_some());
            Ok((200, crate::network::view()))
        }
        (Method::Get, ["api", "answers", id]) => ANSWER_JOBS.plock().get(*id).cloned().map(|v| (200, v)).ok_or((404, format!("unknown answer job {id}"))),
        (Method::Post, ["api", "agents", name, action]) => {
            let s = snap();
            let r = match *action {
                "focus" => op_focus(&s, name),
                "interrupt" => op_interrupt(&s, name),
                "send" => {
                    let images: Vec<String> =
                        j["images"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                    let raw = j["text"].as_str().unwrap_or("");
                    // asText: a leading "!" / "$ " / "/" … stays text instead of a harness command
                    let text = match (j["asText"].as_bool().unwrap_or(false), find(&s.agents, name)) {
                        (true, Some(a)) => crate::commands::as_text(&a.agent_type, raw),
                        _ => raw.to_string(),
                    };
                    // omp btw box open: plain messages wait until it is closed (a "/btw …" is fine)
                    let btw_q = raw.trim_start().strip_prefix("/btw ").map(|q| q.trim().to_string());
                    let btw_open =
                        find(&s.agents, name).and_then(|a| a.handle.clone()).is_some_and(|h| BTW_OPEN.plock().get(&h).is_some_and(|v| v["open"] == true));
                    if btw_open && btw_q.is_none() && !j["force"].as_bool().unwrap_or(false) {
                        return Ok((
                            200,
                            json!({"name": name, "delivered": false, "held": "btw_open", "reason": "btw 창이 열려 있습니다. 먼저 btw 를 닫으세요"}),
                        ));
                    }
                    let force = j["force"].as_bool().unwrap_or(false);
                    let files: Vec<String> = j["files"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                    let mut r = op_send_files(&s, name, &text, &images, &files, force);
                    if let (Ok(v), Some(q)) = (r.as_mut(), btw_q) {
                        v["kind"] = json!("btw");
                        v["btwText"] = json!(q);
                    }
                    // sent anyway, but its provider is unreachable: the UI may confirm first
                    if let (Ok(v), Some(a)) = (r.as_mut(), find(&s.agents, name)) {
                        if crate::network::agent_held(a) {
                            v["warning"] = json!("network_down");
                        }
                    }
                    // an agent's `adash tell`: record the sender (agent→agent message, not human)
                    if let (Ok(v), Some(from)) = (&r, j["from"].as_str().filter(|f| !f.is_empty() && *f != "human")) {
                        if v["delivered"] == true {
                            if let Some(db) = sh.analytics.plock().as_ref() {
                                db.record_agent_send(
                                    now_ms(),
                                    from,
                                    v["name"].as_str().unwrap_or(name),
                                    j["channel"].as_str().unwrap_or("cli-tell"),
                                    j["text"].as_str().unwrap_or(""),
                                );
                            }
                            sh.history.plock().record_agent_send(
                                now_ms(),
                                from,
                                j["text"].as_str().unwrap_or(""),
                                j["channel"].as_str().unwrap_or("cli-tell"),
                                v["name"].as_str().unwrap_or(name),
                            );
                        }
                    }
                    // dashboard chat (no agent `from`) counts as a human interaction
                    if let (Ok(v), true) = (&r, j["from"].as_str().is_none_or(|f| f.is_empty() || f == "human")) {
                        if v["delivered"] == true {
                            if let Some(a) = find(&s.agents, name) {
                                sh.history.plock().record_send(
                                    now_ms(),
                                    &a.identity,
                                    j["text"].as_str().unwrap_or(""),
                                    j["channel"].as_str().unwrap_or("dashboard-chat"),
                                );
                            }
                        }
                    }
                    r
                }
                "rename" => {
                    let r = op_rename(&mut sh.coll.plock(), &s, name, j["name"].as_str().unwrap_or(""));
                    refresh(sh);
                    r
                }
                "iam" => {
                    let r = op_iam(&mut sh.coll.plock(), &s, name, j["name"].as_str().unwrap_or(""), j["role"].as_str());
                    refresh(sh);
                    r
                }
                "request-name" => op_request_names(&mut sh.coll.plock(), &s, &[name.to_string()]),
                "snooze" => {
                    let m = j["minutes"].as_f64().unwrap_or(0.0);
                    if !m.is_finite() || !(0.0..=MAX_SNOOZE_MIN).contains(&m) {
                        return Err((400, format!("minutes must be 0..={MAX_SNOOZE_MIN} (0 = unsnooze)")));
                    }
                    let r = op_snooze(&mut sh.coll.plock(), &s, name, j["minutes"].as_f64().unwrap_or(0.0));
                    refresh(sh);
                    r
                }
                _ => return Err((404, format!("unknown action {action}"))),
            };
            r.map(|v| (200, v)).map_err(bad)
        }
        (Method::Get, ["api", "magi", "roles"]) => crate::magi::roles(&snap(), q("source").as_deref().unwrap_or("")).map(|v| (200, v)).map_err(bad),
        (Method::Post, ["api", "magi"]) => {
            let s = snap();
            let run = crate::magi::prepare(&s, &j)?;
            let mut st = crate::magi::RunState {
                id: run.id.clone(),
                source: run.source.clone(),
                targets: run.targets.clone(),
                target_handles: run.target_handles.clone(),
                kind: run.kind.clone(),
                mode: run.mode.clone(),
                line: run.line.clone(),
                file: run.file.to_string_lossy().into_owned(),
                status: "queued".into(),
                held_reason: None,
                created_at: now_ms(),
                delivered_at: None,
                notified_held: false,
                force: j["force"].as_bool().unwrap_or(false),
                error: None,
                synth_sent_at: None,
            };
            st.save().map_err(|e| (500, e))?;
            // first attempt now; a held run stays queued and the server loop delivers it later
            deliver_magi(&s, &mut st);
            Ok((200, st.view(&threads::load_all())))
        }
        (Method::Get, ["api", "magi", "runs"]) => {
            let ts = threads::load_all();
            Ok((200, json!({"runs": crate::magi::RunState::load_all().iter().take(50).map(|r| r.view(&ts)).collect::<Vec<_>>()})))
        }
        (Method::Post, ["api", "magi", id, act @ ("deliver" | "cancel")]) => {
            let mut st = crate::magi::RunState::load(id).ok_or((404, format!("unknown magi run {id}")))?;
            if st.status == "delivered" {
                return Err((409, "already delivered".into()));
            }
            if *act == "cancel" {
                st.status = "cancelled".into();
                st.save().map_err(|e| (500, e))?;
            } else {
                st.force = true; // 그래도 전달: over a possible draft (never into a working turn)
                deliver_magi(&snap(), &mut st);
            }
            Ok((200, st.view(&threads::load_all())))
        }
        (Method::Post, ["api", "broadcast"]) => {
            // one message to many agents through the normal gate; held ones are reported, never forced
            let s = snap();
            let text = j["text"].as_str().unwrap_or("").to_string();
            if text.trim().is_empty() {
                return Err((400, "text required".into()));
            }
            let targets: Vec<String> = j["targets"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            if targets.is_empty() {
                return Err((400, "targets required".into()));
            }
            let force = j["force"].as_bool().unwrap_or(false);
            let results: Vec<Value> = targets
                .iter()
                .map(|t| match op_send(&s, t, &text, &[], force) {
                    Ok(v) => {
                        if v["delivered"] == true {
                            if let Some(a) = find(&s.agents, t) {
                                sh.history.plock().record_send(now_ms(), &a.identity, &text, "dashboard-broadcast");
                            }
                        }
                        json!({"name": t, "delivered": v["delivered"], "held": v["held"], "mode": v["mode"]})
                    }
                    Err(e) => json!({"name": t, "delivered": false, "held": null, "error": e}),
                })
                .collect();
            let held: Vec<&Value> = results.iter().filter(|r| r["delivered"] != true).collect();
            Ok((200, json!({"text": text, "results": results, "held": held.len()})))
        }
        (Method::Post, ["api", "request-names"]) => {
            let s = snap();
            let r = op_request_names(&mut sh.coll.plock(), &s, &ambiguous_names(&s));
            r.map(|v| (200, v)).map_err(bad)
        }
        (Method::Post, ["api", "ask"]) => {
            let _w = sh.write.plock();
            let opts = threads::ThreadOpts {
                magi_run: j["magiRun"].as_str().map(String::from),
                kind: j["kind"].as_str().map(String::from),
                mode: j["mode"].as_str().map(String::from),
            };
            let t = op_ask(&snap(), j["from"].as_str(), j["to"].as_str().unwrap_or(""), j["request"].as_str().unwrap_or(""), j["thread"].as_str(), &opts)
                .map_err(bad)?;
            Ok((201, t.summary()))
        }
        (Method::Post, ["api", "threads", "register"]) => {
            let s = snap();
            let name_of = |k: &str| find(&s.agents, k).map(|a| a.name.clone()).unwrap_or_else(|| k.to_string());
            let (from, to) = (name_of(j["from"].as_str().unwrap_or("unknown")), name_of(j["to"].as_str().unwrap_or("")));
            let marker = j["marker"].as_str().filter(|m| !m.is_empty()).ok_or((400, "marker required".to_string()))?;
            let _w = sh.write.plock();
            let t = threads::register_external(
                &from,
                &to,
                marker,
                j["brief"].as_str().unwrap_or(""),
                j["kind"].as_str().unwrap_or("review"),
                j["mode"].as_str(),
                now_ms(),
                "hook",
            )
            .map_err(|e| (500, e))?;
            // a helper session (peer review / magi target) without a name: "<requester>-<role>"
            if let (Some(role), Some(target), Some(req)) =
                (j["role"].as_str(), find(&s.agents, j["to"].as_str().unwrap_or("")), find(&s.agents, j["from"].as_str().unwrap_or("")))
            {
                let mut c = sh.coll.plock();
                if c.registry.helper_name(&target.identity, &req.name, role).is_some() {
                    let _ = c.registry.save();
                }
            }
            Ok((201, t.summary()))
        }
        (Method::Post, ["api", "threads", "answered"]) => {
            let _w = sh.write.plock();
            let t = threads::answered_external(j["marker"].as_str().unwrap_or(""), j["text"].as_str().unwrap_or("")).map_err(|e| (404, e))?;
            Ok((200, t.summary()))
        }
        (Method::Post, ["api", "threads", id, "pull"]) => {
            let _w = sh.write.plock();
            let mut t = threads::load(id).ok_or((404, format!("unknown thread {id}")))?;
            let reply = threads::pull(&mut t).map_err(|e| (409, e))?;
            t.save().map_err(|e| (500, e))?;
            Ok((200, json!({"id": t.id, "status": t.status, "via": t.via, "from": t.from, "to": t.to, "reply": reply, "dir": t.dir().to_string_lossy()})))
        }
        (Method::Post, ["api", "threads", id, "deliver-anyway"]) => {
            let _w = sh.write.plock();
            op_deliver_anyway(id).map(|v| (200, v)).map_err(bad)
        }
        (Method::Get, ["api", "threads"]) => Ok((200, json!(threads::load_all().iter().map(threads::Thread::summary).collect::<Vec<_>>()))),
        (Method::Get, ["api", "meta"]) => Ok((200, json!(Meta::load()))),
        (Method::Put, ["api", "meta"]) => {
            let m: Meta = serde_json::from_value(j).map_err(|e| (400, format!("bad meta: {e}")))?;
            let _w = sh.write.plock();
            m.save().map_err(|e| (500, e))?;
            Ok((200, json!(m)))
        }
        (Method::Post, ["api", "human-tasks"]) => {
            let _w = sh.write.plock();
            let mut m = Meta::load();
            let title = j["title"].as_str().unwrap_or("").trim().to_string();
            if title.is_empty() {
                return Err((400, "title required".into()));
            }
            let t = HumanTask {
                id: m.next_task_id(),
                title,
                project: j["project"].as_str().unwrap_or("").into(),
                due: j["due"].as_str().filter(|s| !s.is_empty()).map(String::from),
                done: false,
            };
            m.human_tasks.push(t.clone());
            m.save().map_err(|e| (500, e))?;
            Ok((201, json!(t)))
        }
        (Method::Patch, ["api", "human-tasks", id]) => {
            let _w = sh.write.plock();
            let mut m = Meta::load();
            let t = m.human_tasks.iter_mut().find(|t| t.id == *id).ok_or((404, format!("unknown task {id}")))?;
            if let Some(d) = j["done"].as_bool() {
                t.done = d;
            }
            if let Some(s) = j["title"].as_str() {
                t.title = s.into();
            }
            if j.get("due").is_some() {
                t.due = j["due"].as_str().filter(|s| !s.is_empty()).map(String::from);
            }
            if let Some(s) = j["project"].as_str() {
                t.project = s.into();
            }
            let out = json!(t);
            m.save().map_err(|e| (500, e))?;
            Ok((200, out))
        }
        _ => Err((404, format!("no route {method} /{}", segs.join("/")))),
    }
}

pub fn mime_for(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())?;
    Some(match ext.as_str() {
        "html" => "text/html; charset=utf-8",
        "webmanifest" => "application/manifest+json",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "md" | "markdown" | "txt" | "log" | "tex" | "csv" | "toml" | "yaml" | "yml" | "py" | "rs" | "typ" => "text/plain; charset=utf-8",
        _ => return None,
    })
}

/// Resolution context for one agent: worktree, process cwd, recent tool-call paths, file index.
/// The agent a request means: `?identity=` picks it exactly (a live agent with that identity,
/// else a closed agent rebuilt from its session file), otherwise the live agent by name.
fn agent_for(sh: &Shared, name: &str, identity: Option<&str>) -> Result<crate::collect::Agent, (u16, String)> {
    let snap = sh.snap.plock().clone();
    if let Some(id) = identity.filter(|i| !i.is_empty()) {
        if let Some(a) = snap.agents.iter().find(|a| a.identity == id) {
            return Ok(a.clone());
        }
        let file = std::path::PathBuf::from(id);
        let kind = crate::history::harness_of(&file)
            .filter(|_| file.is_absolute() && file.exists())
            .ok_or((404, format!("no live or closed agent with identity {id}")))?;
        let (cwd, project): (Option<String>, Option<String>) = crate::analytics::open_read()
            .ok()
            .and_then(|db| db.conn.query_row("SELECT cwd, project FROM sessions WHERE file = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).ok())
            .unwrap_or((None, None));
        let cwd = cwd.unwrap_or_default();
        return Ok(crate::collect::Agent {
            name: name.to_string(),
            identity: id.to_string(),
            agent_type: kind.to_string(),
            project: project.unwrap_or_default(),
            worktree_path: cwd.clone(),
            cwd: (!cwd.is_empty()).then(|| std::path::PathBuf::from(&cwd)),
            session_path: Some(file),
            mapping_exact: true,
            mapping_confidence: "exact",
            state: "offline".into(),
            ..Default::default()
        });
    }
    find(&snap.agents, name).cloned().ok_or((404, format!("unknown agent {name}")))
}

fn with_ctx<R>(sh: &Shared, a: &crate::collect::Agent, f: impl FnOnce(&Ctx) -> R) -> R {
    let tool_paths = sh.transcripts.plock().recent_tool_paths(a, 40);
    let ctx = Ctx { worktree: &a.worktree_path, cwd: a.cwd.as_deref(), tool_paths: &tool_paths, index: &sh.files };
    f(&ctx)
}

/// Open a resolved path on the desktop: Markdown with `open.markdownApp` when configured,
/// anything else with the system opener; detached so the app outlives the request.
pub fn op_open(ctx: &Ctx, raw: &str) -> Result<Value, (u16, String)> {
    let r = ctx.resolve(raw)?;
    let canon = r.path;
    let md_app = crate::config::get().open.markdown_app.clone();
    crate::platform::open_path(&canon, md_app.as_deref()).map_err(|e| (500, e))?;
    let is_md = canon.extension().is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"));
    let app = if is_md { md_app.unwrap_or_else(|| crate::platform::opener().into()) } else { crate::platform::opener().into() };
    let mut v = json!({"ok": true, "app": app, "path": canon.to_string_lossy()});
    if r.ambiguous {
        v["ambiguous"] = json!(true);
        v["candidates"] = json!(r.candidates);
    }
    Ok(v)
}

/// Existence/kind for up to 200 paths (UI decides which monospace spans become links).
/// `base` hint: one path or a list (paths mentioned earlier in the same message).
fn bases_of(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        _ => vec![],
    }
}

/// A relative path (no `/`, `~`, `file://`) is first tried against the directory of each
/// base (a base ending in `/` or resolving to a directory is that directory, else its parent);
/// the first that exists wins, else the path is returned unchanged for the normal resolution.
fn with_base(ctx: &Ctx, raw: &str, bases: &[String]) -> String {
    if raw.is_empty() || raw.starts_with('/') || raw.starts_with('~') || raw.starts_with("file://") {
        return raw.to_string();
    }
    for b in bases {
        let dir = match ctx.resolve(b) {
            Ok(r) if r.path.is_dir() => r.path,
            Ok(r) => r.path.parent().map(|p| p.to_path_buf()).unwrap_or(r.path),
            // a base that does not exist itself can still name a directory (".../x.md" → ".../")
            Err(_) => {
                let t = b.trim_end_matches('/');
                let parent = if b.ends_with('/') { t } else { t.rsplit_once('/').map(|x| x.0).unwrap_or("") };
                match ctx.resolve(parent) {
                    Ok(r) => r.path,
                    Err(_) => continue,
                }
            }
        };
        let cand = dir.join(raw.trim_start_matches("./"));
        if cand.exists() {
            if let Ok(r) = ctx.resolve(&cand.to_string_lossy()) {
                return r.path.to_string_lossy().into_owned();
            }
        }
    }
    raw.to_string()
}

pub fn op_stat(ctx: &Ctx, paths: &[Value], bases: &[String]) -> Result<Value, (u16, String)> {
    if paths.len() > 200 {
        return Err((400, "at most 200 paths".into()));
    }
    let results: Vec<Value> = paths
        .iter()
        .map(|p| {
            // a path entry may carry its own base: {path, base}
            let (raw, own) = match p {
                Value::Object(o) => (o.get("path").and_then(Value::as_str).unwrap_or(""), bases_of(o.get("base").unwrap_or(&Value::Null))),
                v => (v.as_str().unwrap_or(""), vec![]),
            };
            let all: Vec<String> = own.into_iter().chain(bases.iter().cloned()).collect();
            let looked = with_base(ctx, raw, &all);
            match ctx.resolve(&looked) {
                Ok(r) => {
                    let mut v =
                        json!({"path": raw, "exists": true, "kind": if r.path.is_dir() { "dir" } else { "file" }, "resolved": r.path.to_string_lossy()});
                    if r.ambiguous {
                        v["ambiguous"] = json!(true);
                        v["candidates"] = json!(r.candidates);
                    }
                    v
                }
                Err(_) => json!({"path": raw, "exists": false, "kind": null, "resolved": null}),
            }
        })
        .collect();
    Ok(json!({"results": results}))
}

fn serve_file(ctx: &Ctx, raw: &str) -> Result<(&'static str, std::path::PathBuf, std::fs::Metadata), (u16, String)> {
    let canon = ctx.resolve(raw)?.path;
    let md = std::fs::metadata(&canon).map_err(|e| (404, e.to_string()))?;
    if !md.is_file() {
        return Err((400, "not a file".into()));
    }
    let ext = canon.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let ct = match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "pdf" | "md" | "markdown" | "txt" => {
            mime_for(&format!("x.{ext}")).unwrap_or("application/octet-stream")
        }
        _ => return Err((415, "unsupported file type".into())),
    };
    if md.len() > FILE_CAP {
        return Err((413, format!("file larger than {} MB", FILE_CAP / 1024 / 1024)));
    }
    Ok((ct, canon, md))
}

#[cfg(test)]
mod retry_tests {
    #[test]
    fn omp_retries_with_f5_others_with_a_message() {
        assert_eq!(super::retry_mode("omp"), "key");
        assert_eq!(super::retry_info("omp")["label"], "F5 재시도");
        for t in ["claude", "codex", "antigravity"] {
            assert_eq!(super::retry_mode(t), "message");
            assert!(super::retry_info(t)["how"].as_str().unwrap().contains(super::RETRY_MESSAGE));
        }
    }
}
