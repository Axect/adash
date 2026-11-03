//! B1 magi buttons: suggest critic/proposer targets and hand the source agent an instruction
//! file that runs the magi-research loop through `adash ask --wait` (so every exchange is a
//! tracked thread labelled with the run id).
use crate::collect::{find, Agent, Snapshot};
use crate::util::{atomic_write, Res};
use serde_json::{json, Value};
use std::path::PathBuf;

pub fn dir() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join(format!("adash-test-magi-{}", std::process::id()));
    }
    crate::platform::data_dir().join("magi")
}

fn is_critic(a: &Agent) -> bool {
    let m = a.model.as_deref().unwrap_or("").to_lowercase();
    a.agent_type == "codex" || (a.agent_type == "omp" && (m.contains("gpt") || m.contains("astra") || m.contains("codex")))
}

fn is_proposer(a: &Agent) -> bool {
    a.agent_type == "antigravity"
}

/// `GET /api/magi/roles?source=`: live candidates in the source's project with one default each.
pub fn roles(snap: &Snapshot, source: &str) -> Res<Value> {
    let src = find(&snap.agents, source).ok_or_else(|| format!("unknown agent {source}"))?;
    let pick = |pred: fn(&Agent) -> bool, prefer: &str| -> Vec<Value> {
        let mut c: Vec<&Agent> = snap.agents.iter().filter(|a| a.identity != src.identity && a.project == src.project && pred(a)).collect();
        // default: the name that says the role (e.g. "galaxy-survey Astra"), else the most recently active
        c.sort_by_key(|a| (!a.name.to_lowercase().contains(prefer), std::cmp::Reverse(a.state_since)));
        c.iter().enumerate().map(|(i, a)| json!({"name": a.name, "agentType": a.agent_type, "model": a.model, "state": a.state, "default": i == 0})).collect()
    };
    let critic = pick(is_critic, "astra");
    let proposer = pick(is_proposer, "argon");
    let mut missing = vec![];
    if critic.is_empty() {
        missing.push("critic");
    }
    if proposer.is_empty() {
        missing.push("proposer");
    }
    Ok(json!({
        "source": src.name, "project": src.project, "critic": critic, "proposer": proposer, "missing": missing,
        "presets": {
            "critic": {"agent": "omp", "model": "openai-codex/gpt-6-astra", "name": format!("{} Astra", src.project)},
            "proposer": {"agent": "antigravity", "model": "gemini-4-argon-medium", "name": format!("{} Argon", src.project),
                         "note": "agy sessions here run with --mode accept-edits --dangerously-skip-permissions"},
        },
    }))
}

fn new_id() -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos() ^ std::process::id();
    format!("M{}-{:04x}", chrono::Local::now().format("%Y%m%d-%H%M%S"), n & 0xffff)
}

/// Skill named in the instruction text: config `magi.skill` (the feature is off without it).
fn skill_path() -> Option<String> {
    crate::config::get().magi.skill.as_deref().map(|p| crate::config::expand(p).to_string_lossy().into_owned())
}

/// Is magi configured? (`/api/state.features.magi`)
pub fn enabled() -> bool {
    skill_path().is_some()
}

pub struct Run {
    pub id: String,
    pub file: PathBuf,
    pub line: String,
    pub source: String,
    pub targets: Vec<String>,
    pub target_handles: Vec<Option<String>>,
    pub kind: String,
    pub mode: String,
}

/// Write the instruction file for `POST /api/magi`; returns the one line to send the source.
pub fn prepare(snap: &Snapshot, j: &Value) -> Result<Run, (u16, String)> {
    let Some(skill) = skill_path() else {
        return Err((501, "magi is off: set magi.skill in config.toml (adash config init)".into()));
    };
    let source = j["source"].as_str().unwrap_or("");
    let src = find(&snap.agents, source).ok_or((404, format!("unknown agent {source}")))?;
    let kind = j["kind"].as_str().unwrap_or("review");
    if !matches!(kind, "review" | "propose" | "both") {
        return Err((400, format!("kind must be review|propose|both, not {kind}")));
    }
    let mode = j["mode"].as_str().unwrap_or(if kind == "propose" { "propose" } else { "accept" });
    if !matches!(mode, "accept" | "discuss" | "propose") {
        return Err((400, format!("mode must be accept|discuss|propose, not {mode}")));
    }
    let names: Vec<String> = j["targets"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    if names.is_empty() {
        return Err((400, "targets required".into()));
    }
    let mut lines = Vec::new();
    for n in &names {
        let t = find(&snap.agents, n).ok_or((404, format!("unknown target {n}")))?;
        if t.identity == src.identity {
            return Err((400, "the source cannot be its own target".into()));
        }
        // proposers always propose; critics use the requested hostile mode
        let (tk, tm) = if is_proposer(t) || kind == "propose" { ("propose", "propose") } else { ("review", if mode == "propose" { "discuss" } else { mode }) };
        lines.push(format!(
            "   - {} ({}{}): `adash ask {} --file <브리프 경로> --wait --timeout 60m --magi-run {{ID}} --kind {tk} --mode {tm}`",
            t.name,
            t.agent_type,
            t.model.as_deref().map(|m| format!(", {m}")).unwrap_or_default(),
            // the terminal handle, not the display name: auto names change and can repeat
            shell_quote(t.handle.as_deref().unwrap_or(&t.name))
        ));
    }
    let id = new_id();
    let note = j["note"].as_str().map(str::trim).filter(|n| !n.is_empty());
    let label = match kind {
        "review" => "검토",
        "propose" => "제안",
        _ => "제안·검토",
    };
    let date = chrono::Local::now().format("%Y-%m-%d %H:%M");
    let body = format!(
        "# adash magi {id} — {label} 요청 ({date})\n\n\
         - 요청: 사용자가 대시보드에서 {label}을 요청했습니다. 모드: {mode}.{note_line}\n\
         - 대상:\n{targets}\n\n\
         ## 할 일\n\
         1. `{skill}`(magi-research)의 규칙을 따르세요: 결정과 파일 수정은 본인이 합니다. 마커는 한 번만 씁니다. 상대가 쓴 숫자와 인용은 모두 파일에서 직접 확인합니다.\n\
         2. 브리프를 직접 쓰세요. 프로젝트의 `_guide/` 안에 둡니다(/tmp 금지). 이름은 `<주제>_{kind}_brief_<날짜>.md`로 합니다. 다음을 넣습니다.\n\
            - 사용자의 최신 요청 원문과 날짜\n\
            - 목표: 독자가 얻어야 할 메시지를 한 문장으로\n\
            - 고정 제약(문체 규칙, 분량, 새 측정 금지 등)과 고정되지 않은 것\n\
            - 읽을 파일 경로와 이전 판 백업 경로\n\
            - 확신이 없는 점을 번호로 매긴 질문 목록\n\
         3. 대상마다 아래 명령을 백그라운드 작업으로 실행하세요(가능하면 동시에). 명령의 `{{ID}}`는 `{id}`입니다. 답은 stdout으로 돌아오고 터미널로 끼어들지 않습니다. 모든 답이 돌아올 때까지 기다린 뒤 4단계로 가세요(턴을 먼저 끝냈다면 adash 가 답이 모였을 때 다시 알려 줍니다).\n\
         {cmds}\n\
         4. 답을 역할별로 합치세요. 검토자의 사실 교정이 제안자보다 우선합니다. 제안자의 대안은 선택지를 넓힙니다. 두 쪽 권고가 다르면 각자의 근거와 함께 둘 다 보여 주고, 본인의 선택을 밝히세요. 평균 내지 마세요.\n\
         5. 사용자에게 보고하세요: 각 대상이 무엇을 제안하거나 교정했는지, 무엇을 골랐고 왜인지, 아직 확인하지 못한 것. 답변 원문은 붙이지 마세요.\n",
        note_line = note.map(|n| format!("\n- 메모: {n}")).unwrap_or_default(),
        targets = names.iter().map(|n| format!("  - {n}")).collect::<Vec<_>>().join("\n"),
        skill = skill,
        cmds = lines.join("\n").replace("{ID}", &id),
    );
    let file = dir().join(format!("{id}.md"));
    atomic_write(&file, body.as_bytes()).map_err(|e| (500, e))?;
    let line = format!("[adash magi {id}] {label} 요청: {} 를 읽고 지시대로 진행하세요.", file.display());
    Ok(Run {
        id,
        file,
        line,
        source: src.name.clone(),
        targets: names.iter().map(|n| find(&snap.agents, n).map(|t| t.name.clone()).unwrap_or_else(|| n.clone())).collect(),
        target_handles: names.iter().map(|n| find(&snap.agents, n).and_then(|t| t.handle.clone())).collect(),
        kind: kind.into(),
        mode: mode.into(),
    })
}

fn shell_quote(s: &str) -> String {
    if s.chars().all(|c| c.is_alphanumeric() || "-_.".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// A magi run waiting for / delivered to its source agent. The instruction line goes through
/// the same gate as chat (never into a working agent or over a draft); held runs retry from
/// the server loop until delivered, like relay threads.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RunState {
    pub id: String,
    pub source: String,
    pub targets: Vec<String>,
    /// terminal handles of the targets (names can change during a run)
    #[serde(default)]
    pub target_handles: Vec<Option<String>>,
    pub kind: String,
    pub mode: String,
    pub line: String,
    pub file: String,
    /// queued | held | delivered | failed | cancelled
    pub status: String,
    pub held_reason: Option<String>,
    pub created_at: i64,
    pub delivered_at: Option<i64>,
    #[serde(default)]
    pub notified_held: bool,
    #[serde(default)]
    pub force: bool,
    pub error: Option<String>,
    /// all answers in, the source idle: one "종합하세요" line was sent (the source may have
    /// ended its turn while its background `adash ask --wait` jobs were still running)
    #[serde(default)]
    pub synth_sent_at: Option<i64>,
}

impl RunState {
    /// Every target answered (reply files present): (latest reply mtime, reply paths).
    pub fn replies(&self, threads: &[crate::threads::Thread]) -> Option<(i64, Vec<String>)> {
        let mine: Vec<&crate::threads::Thread> = threads.iter().filter(|t| t.magi_run.as_deref() == Some(self.id.as_str())).collect();
        if mine.len() < self.targets.len() {
            return None;
        }
        let mut latest = 0;
        let mut paths = vec![];
        for t in mine {
            let p = t.dir().join("reply.md");
            let m = std::fs::metadata(&p).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64;
            latest = latest.max(m);
            paths.push(format!("{} ({})", p.display(), t.to));
        }
        Some((latest, paths))
    }

    pub fn path(id: &str) -> PathBuf {
        dir().join(format!("{id}.json"))
    }
    pub fn save(&self) -> Res<()> {
        atomic_write(&Self::path(&self.id), serde_json::to_string_pretty(self).unwrap().as_bytes())
    }
    pub fn load(id: &str) -> Option<RunState> {
        std::fs::read_to_string(Self::path(id)).ok().and_then(|s| serde_json::from_str(&s).ok())
    }
    pub fn load_all() -> Vec<RunState> {
        let mut v: Vec<RunState> = std::fs::read_dir(dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| std::fs::read_to_string(e.path()).ok().and_then(|s| serde_json::from_str(&s).ok()))
            .collect();
        v.sort_by_key(|r: &RunState| std::cmp::Reverse(r.created_at));
        v
    }
    /// UI view: run status + per-target progress from the threads tagged with this run.
    pub fn view(&self, threads: &[crate::threads::Thread]) -> Value {
        let mine: Vec<&crate::threads::Thread> = threads.iter().filter(|t| t.magi_run.as_deref() == Some(self.id.as_str())).collect();
        let targets: Vec<Value> = self
            .targets
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let h = self.target_handles.get(i).cloned().flatten();
                let t = mine.iter().filter(|t| &t.to == n || (h.is_some() && t.to_handle == h)).max_by_key(|t| t.created_at);
                json!({"name": t.map(|t| t.to.clone()).unwrap_or_else(|| n.clone()), "threadId": t.map(|t| t.id.clone()), "status": t.map(|t| t.status.clone()).unwrap_or_else(|| "not_asked".into())})
            })
            .collect();
        let all_in = self.status == "delivered"
            && !mine.is_empty()
            && targets.iter().all(|t| t["threadId"].as_str().and_then(|id| mine.iter().find(|x| x.id == id)).is_some_and(|x| x.reply_text().is_some()));
        let phase = match self.status.as_str() {
            "delivered" if all_in && self.synth_sent_at.is_some() => "reporting",
            "delivered" if all_in => "answered",
            "delivered" if mine.is_empty() => "delivered",
            "delivered" => "running",
            s => s,
        };
        json!({
            "id": self.id, "source": self.source, "targets": targets, "kind": self.kind, "mode": self.mode,
            "status": self.status, "phase": phase, "heldReason": self.held_reason, "createdAt": self.created_at,
            "deliveredAt": self.delivered_at, "file": self.file, "error": self.error, "synthSentAt": self.synth_sent_at,
        })
    }
}
