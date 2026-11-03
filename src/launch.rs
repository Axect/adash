//! Open projects / git worktrees / agent sessions from adash (agents start in adash tmux
//! terminals, `term::create`), and AI capacity (`omp usage --json --redact`).
use crate::term;
use crate::util::LockExt;
use crate::util::{home, now_ms, Res};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

type ApiErr = (u16, String);

// ---------------------------------------------------------------------------------------------
// workspaces

pub struct Cached {
    at: Option<Instant>,
    value: Value,
}

impl Default for Cached {
    fn default() -> Self {
        Cached { at: None, value: Value::Null }
    }
}

impl Cached {
    pub fn get(&mut self, ttl: Duration, f: impl FnOnce() -> Res<Value>) -> Res<Value> {
        if let Some(at) = self.at {
            if at.elapsed() < ttl {
                return Ok(self.value.clone());
            }
        }
        let v = f()?;
        self.at = Some(Instant::now());
        self.value = v.clone();
        Ok(v)
    }

    pub fn invalidate(&mut self) {
        self.at = None;
    }
}

/// `git -C <dir> worktree list --porcelain` → (path, branch, is main); None outside a git repo.
fn git_worktrees(dir: &Path) -> Option<Vec<(String, Option<String>, bool)>> {
    let out = Command::new("git").arg("-C").arg(dir).args(["worktree", "list", "--porcelain"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let mut v: Vec<(String, Option<String>, bool)> = Vec::new();
    let mut bare = false;
    for l in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(p) = l.strip_prefix("worktree ") {
            let main = v.is_empty() && !bare;
            v.push((p.to_string(), None, main));
        } else if let (Some(b), Some(last)) = (l.strip_prefix("branch "), v.last_mut()) {
            last.1 = Some(b.trim_start_matches("refs/heads/").to_string());
        } else if l == "bare" {
            // a bare repository has no working tree to open
            v.pop();
            bare = true;
        }
    }
    Some(v)
}

/// meta.yaml projects with their git worktrees and live-agent counts (a non-git folder is one
/// entry without a branch).
pub fn workspaces(snap: &crate::collect::Snapshot) -> Res<Value> {
    let meta = crate::meta::Meta::load();
    let same = |a: &str, b: &str| a.trim_end_matches('/') == b.trim_end_matches('/');
    let live = |path: &str| snap.agents.iter().filter(|a| same(&a.worktree_path, path)).count();
    let mut out: Vec<Value> = Vec::new();
    for (name, pm) in &meta.projects {
        let mut seen: Vec<String> = Vec::new();
        for path in &pm.paths {
            // a second path that is a worktree of an earlier one is listed there already
            if seen.iter().any(|s| same(s, path)) || !Path::new(path).is_dir() {
                continue;
            }
            let wts = git_worktrees(Path::new(path)).filter(|w| !w.is_empty()).unwrap_or_else(|| vec![(path.clone(), None, true)]);
            seen.extend(wts.iter().map(|w| w.0.clone()));
            let worktrees: Vec<Value> = wts.iter().map(|(p, b, main)| json!({"path": p, "branch": b, "isMain": main, "liveAgents": live(p)})).collect();
            out.push(json!({"name": name, "path": path, "worktrees": worktrees}));
        }
    }
    Ok(json!({"projects": out}))
}

// ---------------------------------------------------------------------------------------------
// launch catalog

pub fn find_bin(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var("PATH").unwrap_or_default().split(':').map(PathBuf::from).collect();
    dirs.push(home().join(".local/bin"));
    dirs.push(home().join(".bun/bin"));
    // nvm-installed CLIs (codex) are not on the service PATH
    if let Ok(rd) = std::fs::read_dir(home().join(".local/share/nvm")) {
        for e in rd.flatten() {
            dirs.push(e.path().join("bin"));
        }
    }
    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

/// `--model` values of live agent processes of one kind.
fn live_models(kind: &str) -> Vec<String> {
    let mut v: Vec<String> =
        crate::session::scan_procs().values().flatten().filter(|p| p.kind == kind).filter_map(|p| crate::session::arg_value(&p.args, "--model")).collect();
    v.sort();
    v.dedup();
    v
}

/// omp's own catalog (`omp models --json`, chat models) → (selector, thinking levels).
fn omp_models() -> Vec<(String, Vec<String>)> {
    let Some(bin) = find_bin("omp") else { return vec![] };
    let Ok(out) = crate::platform::timeout(20).arg(bin).args(["models", "--json"]).output() else { return vec![] };
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    v["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["kind"] == "chat")
        .filter_map(|m| {
            let sel = m["selector"].as_str()?.to_string();
            let th = m["thinking"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            Some((sel, th))
        })
        .collect()
}

fn toml_model(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()?.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k.trim() == "model").then(|| v.trim().trim_matches('"').to_string())
    })
}

pub fn catalog() -> Value {
    let omp = omp_models();
    let mut omp_list: Vec<String> = omp.iter().map(|m| m.0.clone()).collect();
    for m in live_models("omp") {
        let base = m.split(':').next().unwrap_or(&m).to_string();
        if !omp_list.contains(&base) {
            omp_list.push(base);
        }
    }
    let mut efforts: Vec<String> = Vec::new();
    for (_, th) in &omp {
        for t in th {
            if !efforts.contains(t) {
                efforts.push(t.clone());
            }
        }
    }
    let default_omp = live_models("omp").into_iter().next().map(|m| m.split(':').next().unwrap_or(&m).to_string());
    let mut codex_models = live_models("codex");
    if let Some(m) = toml_model(&home().join(".codex/config.toml")) {
        if !codex_models.contains(&m) {
            codex_models.insert(0, m);
        }
    }
    let claude_models = live_models("claude");
    let agy_models = live_models("antigravity");
    json!({"agents": [
        {"id": "omp", "label": "omp", "installed": find_bin("omp").is_some(), "models": omp_list, "efforts": efforts,
         "effortMode": "suffix", "effortsByModel": omp.iter().map(|(s, t)| (s.clone(), json!(t))).collect::<serde_json::Map<_, _>>(),
         "defaultModel": default_omp},
        {"id": "claude", "label": "Claude Code", "installed": find_bin("claude").is_some(), "models": claude_models, "efforts": [], "effortMode": "none", "defaultModel": null},
        {"id": "codex", "label": "Codex", "installed": find_bin("codex").is_some(), "models": codex_models.clone(), "efforts": ["low", "medium", "high", "xhigh"],
         "effortMode": "flag", "defaultModel": codex_models.first()},
        {"id": "antigravity", "label": "Antigravity", "installed": find_bin("agy").is_some(), "models": agy_models, "efforts": [], "effortMode": "none", "defaultModel": null},
    ]})
}

// ---------------------------------------------------------------------------------------------
// launching

/// Model / effort arguments of the agent command line.
pub fn model_args(agent: &str, model: Option<&str>, effort: Option<&str>) -> Result<Vec<String>, ApiErr> {
    let model = model.map(str::trim).filter(|m| !m.is_empty());
    let effort = effort.map(str::trim).filter(|e| !e.is_empty());
    let mut a = Vec::new();
    match (agent, model, effort) {
        // omp: effort is a model suffix (`--effort` errors for omp)
        ("omp", Some(m), Some(e)) => a.extend(["--model".into(), format!("{}:{e}", m.split(':').next().unwrap_or(m))]),
        ("omp", None, Some(_)) => return Err((400, "omp effort needs a model (it is a model suffix)".into())),
        ("codex", Some(m), Some(e)) => a.extend(["--model".into(), m.to_string(), "--effort".into(), e.to_string()]),
        (_, None, Some(_)) => return Err((400, "--effort requires a model".into())),
        (_, _, Some(_)) => return Err((400, format!("{agent} does not take an effort"))),
        (_, Some(m), None) => a.extend(["--model".into(), m.to_string()]),
        (_, None, None) => {}
    }
    Ok(a)
}

fn agent_ok(agent: &str) -> Result<(), ApiErr> {
    if matches!(agent, "omp" | "claude" | "codex" | "antigravity") {
        Ok(())
    } else {
        Err((400, format!("unknown agent {agent}")))
    }
}

pub struct Launched {
    pub body: Value,
}

/// The shell command that starts `agent` (model flags; the initial prompt as a quoted argument).
fn agent_command(agent: &str, model: Option<&str>, effort: Option<&str>, prompt: Option<&str>) -> Result<String, ApiErr> {
    agent_ok(agent)?;
    let bin = if agent == "antigravity" { "agy" } else { agent };
    let mut parts = vec![bin.to_string()];
    parts.extend(model_args(agent, model, effort)?.iter().map(|a| crate::lifecycle::sq(a)));
    if let Some(p) = prompt.filter(|p| !p.trim().is_empty()) {
        parts.push(crate::lifecycle::sq(p));
    }
    Ok(parts.join(" "))
}

/// `~/x` → `$HOME/x`; absolute paths without `..` only.
fn user_path(raw: &str) -> Result<PathBuf, ApiErr> {
    let path = if let Some(r) = raw.strip_prefix("~/") { home().join(r) } else { PathBuf::from(raw) };
    if !path.is_absolute() || path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err((400, "absolute path without '..' required".into()));
    }
    Ok(path)
}

/// Open a terminal in `dir` (window of `project`'s tmux session) running `cmd`; focus it unless
/// `focus: false`.
/// First prompt of a launched agent by terminal handle (shown as a pending message until the
/// harness writes its session file): handle → (prompt, launched at).
pub static LAUNCH_PROMPTS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, (String, i64)>>> = std::sync::LazyLock::new(Default::default);

fn start(dir: &Path, project: &str, title: &str, cmd: &str, j: &Value, worktree: Option<Value>) -> Result<Launched, ApiErr> {
    let handle = term::create(dir, project, title, Some(cmd), None).map_err(|e| (502, e))?;
    if let Some(p) = j["prompt"].as_str().map(str::trim).filter(|p| !p.is_empty()) {
        let mut m = LAUNCH_PROMPTS.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, v| crate::util::now_ms() - v.1 < 86_400_000);
        m.insert(handle.clone(), (p.to_string(), crate::util::now_ms()));
    }
    let mut body = json!({"handle": handle, "command": cmd});
    if let Some(w) = worktree {
        body["worktree"] = w;
    }
    if j["focus"].as_bool().unwrap_or(true) {
        if let Err(e) = term::focus(&handle) {
            body["focusError"] = json!(e);
        }
    }
    Ok(Launched { body })
}

fn project_for(dir: &Path) -> String {
    let s = dir.to_string_lossy();
    crate::meta::Meta::load().project_for_path(&s).unwrap_or_else(|| crate::collect::project_name("", &s))
}

fn title_for<'a>(j: &'a Value, agent: &'a str) -> &'a str {
    j["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(agent)
}

/// `POST /api/sessions {agent, path, model?, effort?, prompt?, focus?, name?}`
pub fn new_session(j: &Value) -> Result<Launched, ApiErr> {
    let agent = j["agent"].as_str().unwrap_or("omp");
    let cmd = agent_command(agent, j["model"].as_str(), j["effort"].as_str(), j["prompt"].as_str())?;
    let raw = j["path"].as_str().map(str::trim).filter(|p| !p.is_empty()).ok_or((400, "path required".to_string()))?;
    let dir = user_path(raw)?;
    if !dir.is_dir() {
        return Err((404, format!("not a directory: {}", dir.display())));
    }
    start(&dir, &project_for(&dir), title_for(j, agent), &cmd, j, None)
}

/// `POST /api/worktrees {project | path, name, baseBranch?, agent?, model?, effort?, prompt?, focus?}`:
/// `git worktree add -b <name> ~/worktrees/<project>/<name> [baseBranch]`, then an optional agent there.
pub fn new_worktree(j: &Value) -> Result<Launched, ApiErr> {
    let name = j["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or((400, "name required".to_string()))?;
    if name.starts_with(['.', '-']) || name.contains(['/', '\\']) || name.contains(char::is_whitespace) || name.contains("..") {
        return Err((400, format!("invalid worktree name {name}")));
    }
    let meta = crate::meta::Meta::load();
    let (repo, project) = match (j["path"].as_str().map(str::trim).filter(|p| !p.is_empty()), j["project"].as_str().map(str::trim).filter(|p| !p.is_empty())) {
        (Some(p), proj) => {
            let repo = user_path(p)?;
            let s = repo.to_string_lossy().into_owned();
            let proj = proj.map(String::from).or_else(|| meta.project_for_path(&s)).unwrap_or_else(|| crate::collect::project_name("", &s));
            (repo, proj)
        }
        (None, Some(proj)) => {
            let p = meta.projects.get(proj).and_then(|m| m.paths.first()).ok_or((404, format!("unknown project {proj}")))?;
            (PathBuf::from(p), proj.to_string())
        }
        (None, None) => return Err((400, "project or path required".into())),
    };
    // validate the agent request before touching git
    let agent = j["agent"].as_str().filter(|a| !a.is_empty());
    let cmd = agent.map(|a| agent_command(a, j["model"].as_str(), j["effort"].as_str(), j["prompt"].as_str())).transpose()?;
    let dest = home().join("worktrees").join(term::session_name(&project)).join(name);
    if dest.exists() {
        return Err((409, format!("{} exists", dest.display())));
    }
    std::fs::create_dir_all(dest.parent().unwrap()).map_err(|e| (500, format!("mkdir: {e}")))?;
    let mut git = Command::new("git");
    git.arg("-C").arg(&repo).args(["worktree", "add", "-b", name]).arg(&dest);
    if let Some(b) = j["baseBranch"].as_str().map(str::trim).filter(|b| !b.is_empty()) {
        git.arg(b);
    }
    let out = git.output().map_err(|e| (500, format!("git: {e}")))?;
    if !out.status.success() {
        return Err((502, format!("git worktree add: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    let wt = json!({"path": dest, "branch": name});
    match (agent, cmd) {
        (Some(a), Some(c)) => start(&dest, &project, title_for(j, a), &c, j, Some(wt)),
        _ => Ok(Launched { body: json!({"handle": null, "worktree": wt, "command": null}) }),
    }
}

/// `POST /api/projects`: (mkdir under $HOME) (git init) → meta.yaml project → optional agent.
pub fn new_project(j: &Value) -> Result<(Value, Option<Launched>), ApiErr> {
    let raw = j["path"].as_str().map(str::trim).filter(|p| !p.is_empty()).ok_or((400, "path required".to_string()))?;
    let path = user_path(raw)?;
    let home_c = home().canonicalize().map_err(|e| (500, e.to_string()))?;
    if j["create"].as_bool().unwrap_or(false) {
        if !path.starts_with(&home_c) && !path.starts_with(home()) {
            return Err((403, "new folders only under $HOME".into()));
        }
        std::fs::create_dir_all(&path).map_err(|e| (500, format!("mkdir {}: {e}", path.display())))?;
    }
    let canon = path.canonicalize().map_err(|_| (404, format!("not found: {}", path.display())))?;
    if !canon.is_dir() {
        return Err((400, "not a directory".into()));
    }
    let mut steps = vec![];
    if j["gitInit"].as_bool().unwrap_or(false) && !canon.join(".git").exists() {
        let st = Command::new("git").arg("init").arg("-q").arg(&canon).status().map_err(|e| (500, format!("git init: {e}")))?;
        if !st.success() {
            return Err((500, "git init failed".into()));
        }
        steps.push("git init");
    }
    let path_s = canon.to_string_lossy().into_owned();
    // meta.yaml: project with default scores
    let name = canon.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut meta = crate::meta::Meta::load();
    if !meta.projects.values().any(|p| p.paths.iter().any(|x| x == &path_s)) {
        let mut pm = crate::meta::ProjectMeta::default();
        pm.paths.push(path_s.clone());
        meta.projects.entry(name.clone()).or_insert(pm);
        meta.save().map_err(|e| (500, e))?;
        steps.push("meta.yaml");
    }
    let launched = match j["agent"].as_str().filter(|a| !a.is_empty()) {
        Some(agent) => {
            let cmd = agent_command(agent, j["model"].as_str(), j["effort"].as_str(), j["prompt"].as_str())?;
            Some(start(&canon, &name, title_for(j, agent), &cmd, j, None)?)
        }
        None => None,
    };
    Ok((json!({"path": path_s, "project": name, "steps": steps}), launched))
}

// ---------------------------------------------------------------------------------------------
// capacity

#[derive(Default)]
pub struct Capacity {
    value: Option<Value>,
    fetched_at: Option<i64>,
    last_try: Option<Instant>,
    error: Option<String>,
}

fn vendor_of(provider: &str) -> &'static str {
    match provider {
        "anthropic" => "claude",
        p if p.starts_with("openai") => "openai",
        p if p.starts_with("google") => "google",
        _ => "other",
    }
}

/// `omp usage --json --redact` → per-vendor accounts and limits.
fn fetch_usage() -> Res<Value> {
    let bin = find_bin("omp").ok_or("omp not found")?;
    let out = crate::platform::timeout(60).arg(bin).args(["usage", "--json", "--redact"]).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("omp usage exited {:?}: {}", out.status.code(), String::from_utf8_lossy(&out.stderr).chars().take(300).collect::<String>()));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("omp usage JSON: {e}"))
}

pub fn shape_usage(raw: &Value) -> Vec<Value> {
    let mut vendors: Vec<(String, String, Vec<Value>)> = Vec::new(); // (vendor, provider, accounts)
    for r in raw["reports"].as_array().into_iter().flatten() {
        let provider = r["provider"].as_str().unwrap_or("?").to_string();
        let md = &r["metadata"];
        let account = md["email"].as_str().or(md["accountId"].as_str()).or(md["projectId"].as_str()).unwrap_or("default").to_string();
        let limits: Vec<Value> = r["limits"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|l| {
                let a = &l["amount"];
                let used = a["usedFraction"].as_f64().or_else(|| a["remainingFraction"].as_f64().map(|x| 1.0 - x));
                json!({"id": l["id"], "label": l["label"], "windowLabel": l["window"]["label"], "usedFraction": used,
                       "resetsAt": l["window"]["resetsAt"], "status": l["status"], "unit": a["unit"]})
            })
            .collect();
        let acct = json!({"account": account, "plan": md["planType"], "limits": limits});
        match vendors.iter_mut().find(|v| v.1 == provider) {
            Some(v) => v.2.push(acct),
            None => vendors.push((vendor_of(&provider).to_string(), provider, vec![acct])),
        }
    }
    vendors.into_iter().map(|(vendor, provider, accounts)| json!({"vendor": vendor, "provider": provider, "accounts": accounts})).collect()
}

impl Capacity {
    /// Refresh when due: periodic (5 min) or forced (`?refresh=1`, at most once per 60 s).
    pub fn refresh(&mut self, force: bool) -> Option<Value> {
        let due = match self.last_try {
            None => true,
            Some(t) => t.elapsed() >= if force { Duration::from_secs(60) } else { Duration::from_secs(300) },
        };
        if !due {
            return None;
        }
        self.last_try = Some(Instant::now());
        match fetch_usage() {
            Ok(raw) => {
                let vendors = shape_usage(&raw);
                self.value = Some(json!(vendors));
                self.fetched_at = Some(now_ms());
                self.error = None;
                Some(json!(vendors))
            }
            Err(e) => {
                self.error = Some(e);
                None
            }
        }
    }

    pub fn view(&self) -> Value {
        let stale = self.fetched_at.is_none_or(|t| now_ms() - t > 15 * 60_000);
        json!({"fetchedAt": self.fetched_at, "stale": stale, "error": self.error, "vendors": self.value.clone().unwrap_or(json!([]))})
    }
}

/// Capacity poller; also persists every successful fetch into analytics.
pub fn spawn_capacity_poller(cap: &'static Mutex<Capacity>) {
    std::thread::spawn(move || loop {
        let fresh = cap.plock().refresh(false);
        if let Some(v) = fresh {
            persist_capacity(&v);
        }
        std::thread::sleep(Duration::from_secs(30));
    });
}

pub fn persist_capacity(vendors: &Value) {
    if let Ok(db) = crate::analytics::open() {
        let ts = now_ms();
        for v in vendors.as_array().into_iter().flatten() {
            for a in v["accounts"].as_array().into_iter().flatten() {
                for l in a["limits"].as_array().into_iter().flatten() {
                    let _ = db.conn.execute(
                        "INSERT INTO capacity_snapshots(ts, vendor, provider, account, limit_id, label, window_label, used_fraction, resets_at, status)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT DO NOTHING",
                        rusqlite::params![
                            ts,
                            v["vendor"].as_str(),
                            v["provider"].as_str(),
                            a["account"].as_str(),
                            l["id"].as_str(),
                            l["label"].as_str(),
                            l["windowLabel"].as_str(),
                            l["usedFraction"].as_f64(),
                            l["resetsAt"].as_i64(),
                            l["status"].as_str()
                        ],
                    );
                }
            }
        }
    }
}

pub static CAPACITY: std::sync::LazyLock<Mutex<Capacity>> = std::sync::LazyLock::new(|| Mutex::new(Capacity::default()));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_args_follow_agent_mode() {
        assert_eq!(model_args("omp", Some("anthropic/claude-opus-5-5"), Some("low")).unwrap(), vec!["--model", "anthropic/claude-opus-5-5:low"]);
        assert_eq!(model_args("omp", Some("anthropic/claude-opus-5-5:high"), Some("low")).unwrap(), vec!["--model", "anthropic/claude-opus-5-5:low"]);
        assert_eq!(model_args("codex", Some("gpt-6-sol"), Some("high")).unwrap(), vec!["--model", "gpt-6-sol", "--effort", "high"]);
        assert!(model_args("omp", None, Some("low")).is_err());
        assert!(model_args("claude", Some("opus"), Some("low")).is_err());
        assert!(model_args("claude", None, None).unwrap().is_empty());
    }

    #[test]
    fn usage_is_grouped_by_vendor_with_used_fraction() {
        let raw = json!({"reports": [
            {"provider": "anthropic", "metadata": {"email": "ax*", "planType": null}, "limits": [
                {"id": "anthropic:5h", "label": "Claude 5 Hour", "window": {"label": "5 Hour", "resetsAt": 1}, "amount": {"usedFraction": 0.17}, "status": "ok"}]},
            {"provider": "openai-codex", "metadata": {"email": "ax*", "planType": "prolite"}, "limits": []},
            {"provider": "openai-codex", "metadata": {"email": "ax*p*", "planType": "free"}, "limits": []},
            {"provider": "google-antigravity", "metadata": {"projectId": "ai*"}, "limits": [
                {"id": "g:5h", "label": "Gemini", "window": {"label": "5 Hour"}, "amount": {"unit": "percent", "remainingFraction": 0.84}}]},
            {"provider": "zai", "metadata": {}, "limits": []}]});
        let v = shape_usage(&raw);
        let vendors: Vec<&str> = v.iter().map(|x| x["vendor"].as_str().unwrap()).collect();
        assert_eq!(vendors, ["claude", "openai", "google", "other"]);
        assert_eq!(v[1]["accounts"].as_array().unwrap().len(), 2);
        assert!((v[2]["accounts"][0]["limits"][0]["usedFraction"].as_f64().unwrap() - 0.16).abs() < 1e-9);
    }
}
