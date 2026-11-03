//! Tracked ask/reply threads and the relay state machine.
use crate::collect::{find, Agent, Snapshot};
use crate::notify;
use crate::term;
use crate::util::LockExt;
use crate::util::{atomic_write, now_ms, threads_dir, truncate, Res};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

pub const HOP_CAP: u32 = 5;
const CONFIRM_MS: i64 = 20_000;
const SETTLE_MS: i64 = 5_000;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Event {
    pub at: i64,
    pub kind: String,
    #[serde(default)]
    pub detail: Value,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    /// requester name, or "human"
    pub from: String,
    pub to: String,
    pub created_at: i64,
    pub status: String,
    pub hops: u32,
    #[serde(default)]
    pub parent: Option<String>,
    /// "request" (deliver to target) → "relay" (deliver reply to requester) → "done"
    #[serde(default = "phase0")]
    pub phase: String,
    #[serde(default)]
    pub sent_at: Option<i64>,
    #[serde(default)]
    pub confirmed: bool,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub notified_held: bool,
    #[serde(default)]
    pub events: Vec<Event>,
    /// how the reply reached the requester: "push" (terminal line) or "pull" (`adash reply`/`ask --wait`)
    #[serde(default)]
    pub via: Option<String>,
    /// relay phase blocked by the requester: draft | working | asking | unreadable
    #[serde(default)]
    pub held_reason: Option<String>,
    #[serde(default)]
    pub held_since: Option<i64>,
    /// magi run this thread belongs to (B1), its kind (review|propose) and mode (accept|discuss|propose)
    #[serde(default)]
    pub magi_run: Option<String>,
    /// terminal handles at creation: names change (self-naming, auto titles), handles do not
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_handle: Option<String>,
    /// agent identities at creation (session file or pane key): names can be reused
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_identity: Option<String>,
    #[serde(default)]
    pub magi_kind: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    /// registered or detected from an exchange that did not go through `adash ask` (B4)
    #[serde(default)]
    pub inferred: bool,
    #[serde(default)]
    pub marker: Option<String>,
}

/// Optional labels for a new thread.
#[derive(Default, Clone)]
pub struct ThreadOpts {
    pub magi_run: Option<String>,
    pub kind: Option<String>,
    pub mode: Option<String>,
}

fn mode_rules(mode: Option<&str>) -> &'static str {
    match mode {
        Some("accept") => "적대적 검토자로서 판정하세요. 첫 줄에 `수락` 또는 `수정 필요`를 쓰고, 근거와 정확한 수정 문장(파일·위치 포함)을 주세요. 확인하지 못한 것은 확인하지 못했다고 쓰세요.\n\n",
        Some("discuss") => "적대적 검토자로서 논의하세요. 판정 줄 없이 사실·숫자 오류, 주장 범위, 반례, 더 나은 대안을 근거와 함께 쓰세요.\n\n",
        Some("propose") => "제안자로서 답하세요. 현재 형태에 얽매이지 말고 서로 다른 대안 2~3개를 구체 사양, 필요한 사전 지식, 장단점, 위험과 함께 제안하고 하나를 권하세요. 판정 줄(수락/수정 필요)은 쓰지 마세요. 목업은 브리프가 지정한 폴더에만 만드세요.\n\n",
        _ => "",
    }
}

fn phase0() -> String {
    "request".into()
}

pub enum Gate {
    Ok,
    Busy(String),
    Held(String),
}

impl Thread {
    pub fn dir(&self) -> PathBuf {
        threads_dir().join(&self.id)
    }

    pub fn event(&mut self, kind: &str, detail: Value) {
        self.events.push(Event { at: now_ms(), kind: kind.into(), detail });
    }

    pub fn save(&self) -> Res<()> {
        atomic_write(&self.dir().join("thread.json"), serde_json::to_string_pretty(self).unwrap().as_bytes())
    }

    pub fn request_text(&self) -> String {
        let s = std::fs::read_to_string(self.dir().join("request.md")).unwrap_or_default();
        s.split_once("\n## 요청\n").map(|x| x.1.to_string()).unwrap_or(s)
    }

    pub fn reply_text(&self) -> Option<String> {
        std::fs::read_to_string(self.dir().join("reply.md")).ok()
    }

    pub fn summary(&self) -> Value {
        json!({
            "id": self.id, "from": self.from, "to": self.to, "status": self.status, "hops": self.hops,
            "createdAt": self.created_at,
            "request": truncate(self.request_text().split("\n---\n").next().unwrap_or("").trim(), 300),
            "replyPreview": self.reply_text().map(|r| truncate(r.trim(), 300)),
            "dir": self.dir().to_string_lossy(),
            "parent": self.parent, "phase": self.phase,
            "via": self.via, "heldReason": self.held_reason, "heldSince": self.held_since,
            "magiRun": self.magi_run, "magiKind": self.magi_kind, "mode": self.mode, "inferred": self.inferred, "marker": self.marker,
            "toIdentity": self.to_identity, "fromIdentity": self.from_identity,
            "events": self.events,
        })
    }
}

pub fn load_all() -> Vec<Thread> {
    let mut v: Vec<Thread> = std::fs::read_dir(threads_dir())
        .map(|rd| {
            rd.flatten().filter_map(|e| std::fs::read_to_string(e.path().join("thread.json")).ok()).filter_map(|s| serde_json::from_str(&s).ok()).collect()
        })
        .unwrap_or_default();
    v.sort_by_key(|t: &Thread| std::cmp::Reverse(t.created_at));
    v
}

pub fn load(id: &str) -> Option<Thread> {
    let s = std::fs::read_to_string(threads_dir().join(id).join("thread.json")).ok()?;
    serde_json::from_str(&s).ok()
}

fn new_id() -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos() ^ std::process::id().rotate_left(16);
    format!("T{}-{:04x}", chrono::Local::now().format("%Y%m%d-%H%M%S"), n & 0xffff)
}

/// Create a thread (request.md + thread.json). Hop cap applies across `--thread` chains.
pub fn create(from: &str, from_project: &str, to: &str, request: &str, parent: Option<&str>, opts: &ThreadOpts) -> Res<Thread> {
    let hops = match parent {
        Some(p) => load(p).ok_or_else(|| format!("parent thread {p} not found"))?.hops + 1,
        None => 1,
    };
    let mut t = Thread {
        id: new_id(),
        from: from.into(),
        to: to.into(),
        created_at: now_ms(),
        status: "queued".into(),
        hops,
        parent: parent.map(String::from),
        phase: "request".into(),
        sent_at: None,
        confirmed: false,
        force: false,
        notified_held: false,
        via: None,
        held_reason: None,
        held_since: None,
        events: vec![],
        magi_run: opts.magi_run.clone(),
        to_handle: None,
        from_handle: None,
        to_identity: None,
        from_identity: None,
        magi_kind: opts.kind.clone(),
        mode: opts.mode.clone(),
        inferred: false,
        marker: None,
    };
    let dir = t.dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let reply = dir.join("reply.md");
    let proj = if from_project.is_empty() { String::new() } else { format!(" (프로젝트: {from_project})") };
    let md = format!(
        "# adash 검토 요청 {id}\n\n- 요청자: {from}{proj}\n- 대상: {to}\n- 홉: {hops}/{HOP_CAP}{magi}\n\n## 요청\n{request}\n\n---\n\n## 답변 규칙\n{rules}Write your complete answer to `{reply}`, then end your turn with a short summary. Do not ask the requester follow-up questions inside this thread; answer with what you have.\n",
        id = t.id,
        reply = reply.display(),
        magi = opts.magi_run.as_ref().map(|r| format!("\n- magi: {r} ({} · {})", opts.kind.as_deref().unwrap_or("review"), opts.mode.as_deref().unwrap_or("-"))).unwrap_or_default(),
        rules = mode_rules(opts.mode.as_deref()),
    );
    atomic_write(&dir.join("request.md"), md.as_bytes())?;
    t.event("created", json!({"parent": parent}));
    if hops > HOP_CAP {
        t.status = "escalated".into();
        t.event("escalated", json!({"reason": format!("hop cap {HOP_CAP} reached")}));
        notify::send(&format!("adash 스레드 {} 상한 도달", t.id), &format!("{} → {}: 사람이 결정해 주세요", t.from, t.to), None);
    }
    t.save()?;
    Ok(t)
}

/// Inspect the TUI input box. Some(true) = empty, Some(false) = user draft, None = undeterminable.
/// `source` is the read source; only the rendered screen ("screen") is judged.
pub fn composer_empty(agent_type: &str, source: &str, tail: &[String]) -> Option<bool> {
    composer_text(agent_type, source, tail).map(|d| d.is_none())
}

/// The input box content: Some(None) = empty, Some(Some(text)) = a draft, None = unreadable.
pub fn composer_text(agent_type: &str, source: &str, tail: &[String]) -> Option<Option<String>> {
    let is_rule = |l: &str| {
        let t = l.trim();
        t.chars().count() >= 10 && t.chars().all(|c| c == '─' || c == '━')
    };
    let n = tail.iter().rposition(|l| !l.trim().is_empty()).map(|i| i + 1).unwrap_or(0);
    let tail = &tail[..n];
    let some = |s: &str| -> Option<Option<String>> {
        let s = s.trim();
        Some((!s.is_empty()).then(|| s.to_string()))
    };
    if source != "screen" {
        return None;
    }
    if agent_type == "omp" {
        // layout: ─── rule / input lines / ─── rule / status line. Text above the first rule
        // (answer, `※ recap:` block) is history, not input.
        let rules: Vec<usize> = (0..n).filter(|&i| is_rule(&tail[i])).collect();
        if rules.len() >= 2 {
            let (a, b) = (rules[rules.len() - 2], rules[rules.len() - 1]);
            if b + 3 >= n {
                let body: String = tail[a + 1..b].iter().map(|l| l.trim()).collect::<Vec<_>>().join("\n");
                let body = body.trim_start_matches(['>', '›', '❯']).trim();
                // an empty omp box can show a key hint ("<⇧><⇥> to change thinking effort"); it
                // starts with Nerd Font key glyphs (private use area), which typed text never does
                if body.chars().next().is_some_and(|c| matches!(c as u32, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD)) && !body.contains('\n') {
                    return Some(None);
                }
                return some(body);
            }
        }
        return None;
    }
    // claude / codex: last prompt line within the bottom of the screen
    for l in tail.iter().rev().take(10) {
        let t = l.trim().trim_start_matches('│').trim_end_matches('│').trim();
        for p in ['>', '›', '❯'] {
            if let Some(rest) = t.strip_prefix(p) {
                let rest = rest.trim();
                if rest.is_empty() || (agent_type == "claude" && rest.starts_with("Try \"")) {
                    return Some(None);
                }
                return if agent_type == "claude" { some(rest) } else { None };
            }
        }
    }
    None
}

pub fn gate(agent: &Agent) -> Gate {
    // the provider is unreachable: wait, without failing or counting a hop
    if crate::network::agent_held(agent) {
        return Gate::Busy("network".into());
    }
    if agent.state == "asking" {
        // keystrokes would land in the question UI, not the composer
        return Gate::Busy("target asking a question".into());
    }
    if agent.state == "working" {
        return Gate::Busy("target working".into());
    }
    let Some(h) = &agent.handle else { return Gate::Busy("no live terminal".into()) };
    let read = term::screen(h).map(|t| ("screen".to_string(), t)).or_else(|_| term::tail(h));
    match read {
        Err(e) => Gate::Busy(format!("terminal read failed: {e}")),
        Ok((source, tail)) => match composer_empty(&agent.agent_type, &source, &tail) {
            Some(true) => Gate::Ok,
            Some(false) => Gate::Busy("composer has a draft".into()),
            None => Gate::Held("composer state undeterminable".into()),
        },
    }
}

fn deliver(t: &mut Thread, agent: &Agent, line: &str) -> bool {
    let Some(h) = agent.handle.clone().filter(|h| !h.is_empty()) else {
        t.event("send_error", json!({"error": "no terminal handle"}));
        return false;
    };
    let lk = term::send_lock(&h);
    let _g = lk.plock();
    match term::send_line(&h, line) {
        Ok(v) => {
            t.event("sent", json!({"handle": h, "line": line, "receipt": v}));
            true
        }
        Err(e) => {
            t.event("send_error", json!({"handle": h, "error": e}));
            false
        }
    }
}

fn capture_reply(t: &mut Thread, target: Option<&Agent>) {
    let path = t.dir().join("reply.md");
    if path.exists() {
        t.event("answered", json!({"source": "reply.md"}));
    } else {
        let msg = target.map(|a| a.last_message.clone()).unwrap_or_default();
        let body = format!("> adash: 대상이 reply.md를 쓰지 않아 마지막 assistant 메시지로 대체했습니다.\n\n{msg}\n");
        let _ = atomic_write(&path, body.as_bytes());
        t.event("answered", json!({"source": "lastAssistantMessage"}));
    }
    t.status = "answered".into();
    t.sent_at = None;
    t.confirmed = false;
    if t.from == "human" {
        t.phase = "done".into();
        notify::send_for(
            &format!("adash 답변 도착: {}", t.to),
            &truncate(t.reply_text().unwrap_or_default().trim(), 120),
            target.and_then(|a| a.handle.clone()),
            Some(&t.to),
        );
    } else {
        t.phase = "relay".into();
    }
}

/// The live agent for a thread end: by terminal handle first (stable), else by name. A rename
/// is followed so the thread shows the current name.
fn resolve<'a>(snap: &'a Snapshot, name: &mut String, handle: Option<&str>) -> Option<&'a Agent> {
    // a recorded handle is authoritative: when that pane is gone, never fall back to whoever
    // now carries the same name (it may be another agent)
    let a = match handle {
        Some(h) => snap.agents.iter().find(|a| a.handle.as_deref() == Some(h))?,
        None => find(&snap.agents, name).filter(|a| snap.agents.iter().filter(|x| x.name == a.name).count() == 1)?,
    };
    if a.name != *name && name != "human" {
        *name = a.name.clone();
    }
    Some(a)
}

/// Advance one thread given a fresh snapshot. Returns true when it changed.
pub fn step(t: &mut Thread, snap: &Snapshot) -> bool {
    // compare the whole record: flags like notified_held must be persisted too (else re-notify every poll)
    let before = serde_json::to_string(&*t).unwrap_or_default();
    let now = now_ms();
    match t.phase.as_str() {
        "request" => {
            let target = resolve(snap, &mut t.to, t.to_handle.as_deref());
            if let Some(sent) = t.sent_at {
                // delivered / delivery_unconfirmed: watch target
                let Some(a) = target else { return false };
                let moved = a.state_since > sent;
                if !t.confirmed && (a.state == "working" || moved) {
                    t.confirmed = true;
                    t.event("confirmed_working", json!({}));
                    if t.status == "delivery_unconfirmed" {
                        t.status = "delivered".into();
                    }
                }
                let reply_ready = t.dir().join("reply.md").exists();
                // settled: the state can flicker to non-working between tool calls
                let settled = now - a.state_since >= SETTLE_MS;
                if a.state != "working" && a.state != "idle" && settled && (moved || reply_ready) && (t.confirmed || reply_ready) {
                    capture_reply(t, Some(a));
                } else if !t.confirmed && t.status == "delivered" && now - sent > CONFIRM_MS {
                    t.status = "delivery_unconfirmed".into();
                    t.event("delivery_unconfirmed", json!({}));
                }
            } else if matches!(t.status.as_str(), "queued" | "held_possible_draft") {
                let Some(a) = target else { return false };
                let line = format!("[adash {} from {}] 검토 요청: {}/request.md 를 읽고 지시대로 답해 주세요.", t.id, t.from, t.dir().display());
                try_send(t, a, &line, "delivered");
                if t.status == "delivered" {
                    t.sent_at = Some(now);
                }
            }
        }
        "relay" => {
            if t.hops + 1 > HOP_CAP {
                t.status = "escalated".into();
                t.phase = "done".into();
                t.event("escalated", json!({"reason": "hop cap reached before relay"}));
                notify::send(&format!("adash 스레드 {} 상한 도달", t.id), &format!("{} → {}: 사람이 결정해 주세요", t.to, t.from), None);
            } else if !matches!(t.status.as_str(), "answered" | "held_possible_draft" | "relay_held" | "failed") || (t.status == "failed" && !t.force) {
                // failed: wait for deliver-anyway
            } else if let Some(a) = resolve(snap, &mut t.from, t.from_handle.as_deref()) {
                let line = format!("[adash {} reply from {}] 답변: {}/reply.md", t.id, t.to, t.dir().display());
                let g = if t.force && a.state != "working" && a.state != "asking" && a.handle.is_some() { Gate::Ok } else { gate(a) };
                let (req, handle) = (a.name.clone(), a.handle.clone());
                let reason = held_reason(&g);
                if matches!(reason, "draft" | "unreadable") && t.held_reason.as_deref() != Some(reason) {
                    if let Some(h) = &handle {
                        term::snapshot(h, "relay-held");
                    }
                }
                relay_on_gate(t, g, now, &req, || deliver_line(&handle, &line));
            }
        }
        _ => {}
    }
    serde_json::to_string(&*t).unwrap_or_default() != before
}

fn deliver_line(handle: &Option<String>, line: &str) -> Result<Value, String> {
    term::send_line(handle.as_deref().unwrap_or(""), line)
}

/// Gate refusal → held reason shown to the user.
pub fn held_reason(g: &Gate) -> &'static str {
    match g {
        Gate::Ok => "",
        Gate::Busy(r) if r.contains("working") => "working",
        Gate::Busy(r) if r.contains("asking") => "asking",
        Gate::Busy(r) if r.contains("draft") => "draft",
        Gate::Busy(r) if r == "network" => "network",
        _ => "unreadable",
    }
}

const RELAY_NOTIFY_AFTER_MS: i64 = 60_000;

/// Relay phase, one attempt per poll: deliver the pointer line when the requester's gate is open
/// (or forced), otherwise park the thread as `relay_held` with the reason and notify once after 60 s.
pub fn relay_on_gate(t: &mut Thread, g: Gate, now: i64, requester: &str, send: impl FnOnce() -> Result<Value, String>) {
    if matches!(g, Gate::Ok) {
        match send() {
            Ok(v) => {
                t.event("sent", json!({"line": "reply pointer", "receipt": v, "forced": t.force}));
                t.status = "relayed".into();
                t.via = Some("push".into());
                t.hops += 1;
                t.phase = "done".into();
                t.force = false;
                t.held_reason = None;
            }
            Err(e) => {
                t.event("send_error", json!({"error": e}));
                t.status = "failed".into();
                t.force = false;
            }
        }
        return;
    }
    let reason = held_reason(&g);
    if t.status != "relay_held" || t.held_reason.as_deref() != Some(reason) {
        t.status = "relay_held".into();
        t.held_reason = Some(reason.into());
        t.held_since.get_or_insert(now);
        t.event("relay_held", json!({"reason": reason}));
    }
    if !t.notified_held && now - t.held_since.unwrap_or(now) > RELAY_NOTIFY_AFTER_MS {
        t.notified_held = true;
        let why = match reason {
            "draft" => "입력창에 초안",
            "working" => "작업 중",
            "asking" => "질문 대기 중",
            _ => "입력창 상태 확인 불가",
        };
        notify::send(
            &format!("검토 답변 보류: {requester} {why}"),
            &format!("{} 의 답변이 전달되지 않았습니다 ({}). 대시보드에서 [초안과 함께 전달] 또는 `adash reply {}`", t.to, t.id, t.id),
            None,
        );
    }
}

/// The requester fetched the reply itself (`adash reply` / `ask --wait`): close the thread so
/// the push path never injects it afterwards. Returns the reply text.
pub fn pull(t: &mut Thread) -> Result<String, String> {
    let reply = t.reply_text().ok_or_else(|| format!("{} not answered yet (status {})", t.id, t.status))?;
    // whoever pulls the reply (adash ask --wait / adash reply) has delivered it: close the thread
    // even if the relay loop has not seen the reply yet (else it later holds a relay nobody needs)
    if t.phase != "done" {
        if t.from != "human" {
            t.hops += 1;
        }
        t.status = "relayed".into();
        t.via = Some("pull".into());
        t.phase = "done".into();
        t.held_reason = None;
        t.event("pulled", json!({}));
    }
    Ok(reply)
}

fn try_send(t: &mut Thread, a: &Agent, line: &str, ok_status: &str) {
    let g = if t.force && a.state != "working" && a.state != "asking" && a.handle.is_some() { Gate::Ok } else { gate(a) };
    match g {
        Gate::Ok => {
            if deliver(t, a, line) {
                t.status = ok_status.into();
                t.force = false;
            } else {
                t.status = "failed".into();
            }
        }
        Gate::Busy(r) => {
            if t.status == "held_possible_draft" {
                return;
            }
            if t.events.last().map(|e| e.kind.as_str()) != Some("waiting") {
                if r.contains("draft") {
                    if let Some(h) = &a.handle {
                        term::snapshot(h, "request-held-draft");
                    }
                }
                t.event("waiting", json!({"reason": r}));
            }
        }
        Gate::Held(r) => {
            if t.status != "held_possible_draft" {
                t.status = "held_possible_draft".into();
                t.event("held_possible_draft", json!({"reason": r, "target": a.name}));
                if let Some(h) = &a.handle {
                    term::snapshot(h, "request-held");
                }
            }
            if !t.notified_held {
                t.notified_held = true;
                notify::send(
                    &format!("adash 전송 보류: {}", a.name),
                    &format!("입력창 상태를 판단할 수 없어 {} 전송을 보류했습니다 (그래도 전송 가능)", t.id),
                    a.handle.clone(),
                );
            }
        }
    }
}

/// B4: an exchange that did not go through `adash ask` (peer_review.py / magi.py hook, or
/// detected from a pointer line). Tracked for the timeline only: `phase: "external"`, so the
/// relay loop never sends anything for it.
pub fn register_external(from: &str, to: &str, marker: &str, brief: &str, kind: &str, mode: Option<&str>, at: i64, inferred_from: &str) -> Res<Thread> {
    if let Some(t) = load_all().into_iter().find(|t| t.marker.as_deref() == Some(marker) && t.to == to) {
        return Ok(t); // one thread per marker and target
    }
    let mut t = Thread {
        id: new_id(),
        from: from.into(),
        to: to.into(),
        created_at: at,
        status: "delivered".into(),
        hops: 1,
        parent: None,
        phase: "external".into(),
        sent_at: Some(at),
        confirmed: true,
        force: false,
        notified_held: false,
        events: vec![],
        via: None,
        held_reason: None,
        held_since: None,
        magi_run: None,
        to_handle: None,
        from_handle: None,
        to_identity: None,
        from_identity: None,
        magi_kind: Some(kind.into()),
        mode: mode.map(String::from),
        inferred: true,
        marker: Some(marker.into()),
    };
    std::fs::create_dir_all(t.dir()).map_err(|e| e.to_string())?;
    let md = format!(
        "# {kind} ({}) — [{marker}]\n\n- 요청자: {from}\n- 대상: {to}\n- 브리프: {brief}\n- 경로: {inferred_from}\n\n## 요청\n{brief} 를 읽고 답해줘.\n",
        mode.unwrap_or("-")
    );
    atomic_write(&t.dir().join("request.md"), md.as_bytes())?;
    t.event("registered", json!({"source": inferred_from}));
    t.save()?;
    Ok(t)
}

/// The script received the answer: keep it as reply.md and close the thread.
pub fn answered_external(marker: &str, text: &str) -> Res<Thread> {
    let mut t = load_all().into_iter().find(|t| t.marker.as_deref() == Some(marker)).ok_or_else(|| format!("no thread with marker [{marker}]"))?;
    if !text.trim().is_empty() {
        atomic_write(&t.dir().join("reply.md"), text.as_bytes())?;
    }
    if t.status != "relayed" {
        t.status = "relayed".into();
        t.via = Some("pull".into());
        t.phase = "done".into();
        t.event("answered", json!({"source": "external"}));
        t.save()?;
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::composer_empty;

    fn v(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }
    const RULE: &str = "──────────────────────────────────────────";

    #[test]
    fn omp_empty_composer() {
        let t = v(&[" ✅ done.", RULE, RULE, " · Opus 5.5 · ~/x · main", "", ""]);
        assert_eq!(composer_empty("omp", "screen", &t), Some(true));
    }

    #[test]
    fn omp_draft_is_detected() {
        let t = v(&[" answer", RULE, " half-typed question", RULE, " · Opus 5.5 · ~/x"]);
        assert_eq!(composer_empty("omp", "screen", &t), Some(false));
    }

    /// Bottom of BRIEF's own terminal when adash held a send as possible_draft (2026-10-28):
    /// final answer + `※ recap:` block, then an EMPTY input box; the box must be recognised
    /// as empty.
    fn brief_idle_with_recap() -> Vec<String> {
        v(&[
            " - 커서: m<개수>:<파일태그> 형식입니다. 세션 파일이 바뀌면 처음부터 다시 받습니다.",
            " - 중단: omp/claude/codex에는 --interrupt 대신 Esc를 보냅니다.",
            "",
            " ※ recap: 관측 일정 도구의 백엔드를 만드는 중이며, 대화 보기 기능을 구현해 커밋까지 마쳤습니다.",
            " 다음은 PLANNER가 리뷰한 뒤 다음 작업을 줄 차례입니다.",
            "",
            "──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────",
            "",
            "──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────",
            " \u{f0d57} · \u{f0a9f} Opus 5.5 · \u{f115} ~/work/galaxy-survey · \u{f126} master · \u{e70f} 29.2%/1M \u{f0068} · \u{f067a} 10.98            \u{f014d} PLANNER: Complete the Core Brief Task",
            "",
        ])
    }

    #[test]
    fn recap_block_with_empty_box_is_empty() {
        let t = brief_idle_with_recap();
        assert_eq!(composer_empty("omp", "screen", &t), Some(true));
    }

    #[test]
    fn unrendered_or_unknown_is_undeterminable() {
        let t = v(&["omp --model x", "", ""]);
        assert_eq!(composer_empty("omp", "screen", &t), None);
        assert_eq!(composer_empty("codex", "screen", &v(&["› Find and fix a bug"])), None);
        assert_eq!(composer_empty("claude", "screen", &v(&["│ > │"])), Some(true));
    }

    fn relay_thread() -> super::Thread {
        super::Thread {
            id: "T-test".into(),
            from: "REQ".into(),
            to: "TGT".into(),
            created_at: 0,
            status: "answered".into(),
            hops: 1,
            parent: None,
            phase: "relay".into(),
            sent_at: None,
            confirmed: false,
            force: false,
            notified_held: false,
            events: vec![],
            via: None,
            held_reason: None,
            held_since: None,
            magi_run: None,
            to_handle: None,
            from_handle: None,
            to_identity: None,
            from_identity: None,
            magi_kind: None,
            mode: None,
            inferred: false,
            marker: None,
        }
    }

    #[test]
    fn relay_is_held_with_reason_then_delivered_once() {
        use super::{relay_on_gate, Gate};
        let mut t = relay_thread();
        let mut sends = 0;
        relay_on_gate(&mut t, Gate::Busy("composer has a draft".into()), 1_000, "REQ", || {
            sends += 1;
            Ok(serde_json::json!({}))
        });
        assert_eq!((t.status.as_str(), t.held_reason.as_deref(), t.held_since, t.notified_held), ("relay_held", Some("draft"), Some(1_000), false));
        // still held a minute later: notify once, keep the original heldSince
        relay_on_gate(&mut t, Gate::Busy("composer has a draft".into()), 62_000, "REQ", || {
            sends += 1;
            Ok(serde_json::json!({}))
        });
        assert!(t.notified_held);
        assert_eq!(t.held_since, Some(1_000));
        relay_on_gate(&mut t, Gate::Busy("target working".into()), 63_000, "REQ", || {
            sends += 1;
            Ok(serde_json::json!({}))
        });
        assert_eq!(t.held_reason.as_deref(), Some("working"));
        // composer clears → delivered by push exactly once
        relay_on_gate(&mut t, Gate::Ok, 70_000, "REQ", || {
            sends += 1;
            Ok(serde_json::json!({}))
        });
        assert_eq!((t.status.as_str(), t.phase.as_str(), t.via.as_deref(), t.hops, sends), ("relayed", "done", Some("push"), 2, 1));
    }

    #[test]
    fn pull_wins_over_push_and_blocks_later_injection() {
        use super::{pull, relay_on_gate, Gate};
        let mut t = relay_thread();
        t.id = format!("T-pull-test-{}", std::process::id());
        let dir = t.dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("reply.md"), "답변 본문").unwrap();
        relay_on_gate(&mut t, Gate::Busy("composer has a draft".into()), 1_000, "REQ", || Ok(serde_json::json!({})));
        assert_eq!(pull(&mut t).unwrap(), "답변 본문");
        assert_eq!((t.status.as_str(), t.phase.as_str(), t.via.as_deref(), t.hops), ("relayed", "done", Some("pull"), 2));
        // a second pull returns the reply without counting another hop
        assert_eq!(pull(&mut t).unwrap(), "답변 본문");
        assert_eq!(t.hops, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pull_before_answer_is_refused() {
        let mut t = relay_thread();
        t.id = "T-nonexistent-for-test".into();
        t.phase = "request".into();
        t.status = "delivered".into();
        assert!(super::pull(&mut t).is_err());
    }
}
#[cfg(test)]
mod composer_hint_tests {
    /// Fresh omp session (2026-10-29): the empty box shows a key hint, read as a "draft" — the
    /// false hold that kept magi instructions away from CDF.
    #[test]
    fn omp_key_hint_in_an_empty_box_is_not_a_draft() {
        let screen: Vec<String> = [
            " Connected to MCP servers: slack.",
            "──────────────────────",
            " \u{f0636} \u{f0312} to change thinking effort",
            "──────────────────────",
            " \u{f0d57} · Haiku 4.5 · …/adash-dev",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(super::composer_text("omp", "screen", &screen), Some(None));
        let typed: Vec<String> = ["──────────────────────", " 검토 부탁해요", "──────────────────────", " status"].iter().map(|s| s.to_string()).collect();
        assert_eq!(super::composer_text("omp", "screen", &typed), Some(Some("검토 부탁해요".into())));
    }
}
