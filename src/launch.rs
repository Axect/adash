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
    // board projects working in a folder no entry above covers (a subfolder of a repository, a
    // folder outside meta.yaml): listed as their own entry, under the enclosing one if any
    let covered = |p: &str| out.iter().any(|e| e["worktrees"].as_array().into_iter().flatten().any(|w| w["path"].as_str().is_some_and(|x| same(x, p))));
    let mut extra: Vec<(i64, Value)> = vec![];
    let mut done: Vec<String> = vec![];
    for a in &snap.agents {
        let p = a.worktree_path.trim_end_matches('/').to_string();
        if p.is_empty() || covered(&p) || done.contains(&p) || !Path::new(&p).is_dir() {
            continue;
        }
        done.push(p.clone());
        let parent = out
            .iter()
            .filter_map(|e| Some((e["name"].as_str()?.to_string(), e["path"].as_str()?.trim_end_matches('/').to_string())))
            .filter(|(_, ep)| p.starts_with(&format!("{ep}/")))
            .max_by_key(|(_, ep)| ep.len());
        let used = snap.agents.iter().filter(|b| same(&b.worktree_path, &p)).map(|b| b.state_since).max().unwrap_or(0);
        extra.push((
            used,
            json!({"name": a.project, "path": p, "parent": parent.as_ref().map(|x| &x.0), "subPath": parent.as_ref().and_then(|x| p.strip_prefix(&format!("{}/", x.1))),
                   "worktrees": [{"path": p, "branch": null, "isMain": true, "liveAgents": live(&p)}]}),
        ));
    }
    extra.sort_by_key(|x| std::cmp::Reverse(x.0));
    out.extend(extra.into_iter().map(|x| x.1));
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

fn toml_model(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()?.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k.trim() == "model").then(|| v.trim().trim_matches('"').to_string())
    })
}

/// Claude Code's default model (`model` in its settings.json).
fn claude_default() -> Option<String> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(home().join(".claude/settings.json")).ok()?).ok()?;
    v["model"].as_str().map(String::from)
}

/// The new-session dialog's per-harness models, efforts per model and default model. The lists
/// are the ones the model switch uses (harness binaries, cached an hour); a harness whose list
/// cannot be read falls back to the `--model` values of its running agents.
pub fn catalog() -> Value {
    let entry = |id: &str, label: &str, bin: &str, mode: &str, default: Option<String>| {
        let mut list = crate::model_switch::models_of(id);
        if list.is_empty() {
            list = live_models(id).into_iter().map(|m| crate::model_switch::ModelOpt { label: m.clone(), id: m, efforts: vec![] }).collect();
        }
        if id == "omp" {
            for m in live_models("omp") {
                let base = m.split(':').next().unwrap_or(&m).to_string();
                if !list.iter().any(|x| x.id == base) {
                    list.push(crate::model_switch::ModelOpt { label: base.clone(), id: base, efforts: vec![] });
                }
            }
        }
        let mut efforts: Vec<String> = vec![];
        for e in list.iter().flat_map(|m| m.efforts.iter()) {
            if !efforts.contains(e) {
                efforts.push(e.clone());
            }
        }
        let default = default.or_else(|| list.first().map(|m| m.id.clone()));
        json!({
            "id": id, "label": label, "installed": find_bin(bin).is_some(),
            "models": list.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
            "labels": list.iter().map(|m| (m.id.clone(), json!(m.label))).collect::<serde_json::Map<_, _>>(),
            "efforts": efforts, "effortMode": mode,
            "effortsByModel": list.iter().map(|m| (m.id.clone(), json!(m.efforts))).collect::<serde_json::Map<_, _>>(),
            "defaultModel": default,
        })
    };
    let omp_default = live_models("omp").into_iter().next().map(|m| m.split(':').next().unwrap_or(&m).to_string());
    json!({"agents": [
        entry("omp", "omp", "omp", "suffix", omp_default),
        entry("claude", "Claude Code", "claude", "flag", claude_default()),
        entry("codex", "Codex", "codex", "flag", toml_model(&home().join(".codex/config.toml"))),
        entry("antigravity", "Antigravity", "agy", "dash-suffix", None),
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
        ("codex" | "claude", Some(m), Some(e)) => a.extend(["--model".into(), m.to_string(), "--effort".into(), e.to_string()]),
        // Antigravity names the level in the model id (`gemini-4-argon-medium`)
        ("antigravity", Some(m), Some(e)) => a.extend(["--model".into(), format!("{m}-{e}")]),
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
        // agy reads a first prompt only from `-i` (a bare argument is refused)
        if agent == "antigravity" {
            parts.push("-i".into());
        }
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

/// Terminals the dashboard started that may accept their harness's trust screen
/// (`autoTrust`, default on; `[launch] autoTrust` turns it off everywhere).
pub static AUTO_TRUST: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::LazyLock::new(Default::default);

fn start(dir: &Path, project: &str, title: &str, cmd: &str, j: &Value, worktree: Option<Value>) -> Result<Launched, ApiErr> {
    let handle = term::create(dir, project, title, Some(cmd), None).map_err(|e| (502, e))?;
    if j["autoTrust"] != false {
        AUTO_TRUST.lock().unwrap_or_else(|e| e.into_inner()).insert(handle.clone());
        // also on the pane, so it survives an adash restart
        let _ = term::set_pane_opt(&handle, "@adash_trust", "1");
    }
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
    /// the last good usage per harness read from the harness itself (agy, claude, codex)
    own: std::collections::HashMap<String, Value>,
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

/// Antigravity's own `/usage` (`agy -p "/usage" --output-format json`): no model call. Each group
/// (Gemini models, Claude and GPT models) becomes an account of provider `google-antigravity`.
pub fn agy_usage_shape(v: &Value) -> Option<Value> {
    let groups = v["command"]["data"]["groups"].as_array()?;
    let accounts: Vec<Value> = groups
        .iter()
        .map(|g| {
            let name = g["name"].as_str().unwrap_or("Antigravity");
            let label = if name.starts_with("Gemini") { "Gemini".to_string() } else if name.starts_with("Claude") { "Claude·GPT via Antigravity".to_string() } else { name.to_string() };
            let limits: Vec<Value> = g["buckets"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|b| {
                    let w = match b["window"].as_str() {
                        Some("weekly") => "7d",
                        Some(x) => x,
                        None => "",
                    };
                    let reset = b["reset_time"].as_str().and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok()).map(|t| t.timestamp_millis());
                    json!({"id": b["id"], "label": b["name"], "windowLabel": w, "usedFraction": b["remaining_fraction"].as_f64().map(|r| (1.0 - r).max(0.0)), "resetsAt": reset, "status": null, "unit": "percent"})
                })
                .collect();
            json!({"account": label, "plan": null, "limits": limits})
        })
        .collect();
    (!accounts.is_empty()).then(|| json!({"vendor": "google", "provider": "google-antigravity", "source": "agy", "accounts": accounts}))
}

/// Claude Code's `/usage` text: lines `Current week (all models): 34% used · resets Nov 14, 12pm
/// (Asia/Shanghai)`. Each `(label)` becomes an account; unknown lines are ignored. The reset is kept
/// as Claude prints it (`resetsText`).
pub fn claude_usage_shape(text: &str) -> Option<Value> {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^\s*(Current [^:(]+?)(?:\s*\(([^)]*)\))?:\s*(\d+(?:\.\d+)?)% used(?:\s*·\s*resets\s*(.+?))?\s*$").unwrap()
    });
    let mut accounts: Vec<(String, Vec<Value>)> = vec![];
    for c in text.lines().filter_map(|l| RE.captures(l)) {
        let win = c[1].trim().to_lowercase();
        let w = if win.contains("week") {
            "7d"
        } else if win.contains("session") || win.contains("5") {
            "5h"
        } else {
            win.as_str()
        }
        .to_string();
        let acct = c.get(2).map(|m| m.as_str().to_string()).unwrap_or_else(|| "all models".into());
        let used = c[3].parse::<f64>().ok().map(|p| p / 100.0);
        let lim = json!({"id": format!("claude:{w}:{acct}"), "label": c[1].trim(), "windowLabel": w, "usedFraction": used, "resetsAt": null, "resetsText": c.get(4).map(|m| m.as_str()), "status": null, "unit": "percent"});
        match accounts.iter_mut().find(|a| a.0 == acct) {
            Some(a) => a.1.push(lim),
            None => accounts.push((acct, vec![lim])),
        }
    }
    (!accounts.is_empty()).then(|| {
        json!({"vendor": "claude", "provider": "anthropic", "source": "claude", "accounts": accounts.into_iter().map(|(a, l)| json!({"account": a, "plan": null, "limits": l})).collect::<Vec<_>>()})
    })
}

/// `claude -p /usage` without a session file, in an empty folder of adash's data directory.
fn fetch_claude_usage() -> Res<Value> {
    let bin = find_bin("claude").ok_or("claude not installed")?;
    let dir = crate::platform::data_dir().join("usage-run");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let out = crate::platform::timeout(35)
        .arg(&bin)
        .args(["-p", "/usage", "--no-session-persistence", "--output-format", "json"])
        .current_dir(&dir)
        .output()
        .map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("claude /usage: {e}"))?;
    claude_usage_shape(v["result"].as_str().unwrap_or("")).ok_or_else(|| "claude /usage: no usage lines".to_string())
}

/// Codex: the newest `rate_limits` in its session files (no process started); stale after 6 h.
pub fn codex_usage_shape(rl: &Value, at: i64) -> Option<Value> {
    let limits: Vec<Value> = ["primary", "secondary"]
        .iter()
        .filter_map(|k| {
            let x = &rl[*k];
            x.is_object().then(|| {
                let mins = x["window_minutes"].as_i64().unwrap_or(0);
                let w = if mins >= 1440 { format!("{}d", mins / 1440) } else { format!("{}h", mins / 60) };
                json!({"id": format!("codex:{k}"), "label": w, "windowLabel": w, "usedFraction": x["used_percent"].as_f64().map(|p| p / 100.0), "resetsAt": x["resets_at"].as_i64().map(|s| s * 1000), "status": null, "unit": "percent"})
            })
        })
        .collect();
    (!limits.is_empty()).then(|| {
        json!({"vendor": "openai", "provider": "openai-codex", "source": "codex-session", "seenAt": at, "stale": now_ms() - at > 6 * 3_600_000,
               "accounts": [{"account": "Codex", "plan": rl["plan_type"], "limits": limits}]})
    })
}

fn fetch_codex_usage() -> Res<Value> {
    let root = crate::util::home().join(".codex/sessions");
    let mut newest: Option<(i64, std::path::PathBuf)> = None;
    for back in 0..14 {
        let d = root.join((chrono::Local::now() - chrono::Duration::days(back)).format("%Y/%m/%d").to_string());
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let m = crate::util::mtime_ms(&e.path());
            if newest.as_ref().is_none_or(|n| m > n.0) {
                newest = Some((m, e.path()));
            }
        }
        if newest.is_some() {
            break;
        }
    }
    let (m, f) = newest.ok_or("no Codex session files")?;
    let text = std::fs::read_to_string(&f).map_err(|e| e.to_string())?;
    let rl = text
        .lines()
        .rev()
        .filter(|l| l.contains("\"rate_limits\""))
        .find_map(|l| serde_json::from_str::<Value>(l).ok().map(|v| v["payload"]["rate_limits"].clone()))
        .ok_or("no rate limits in the newest Codex session")?;
    codex_usage_shape(&rl, m).ok_or_else(|| "Codex rate limits without windows".into())
}

/// Run `agy /usage` (30 s cap) in an empty folder of adash's data directory.
fn fetch_agy_usage() -> Res<Value> {
    let bin = find_bin("agy").ok_or("agy not installed")?;
    let dir = crate::platform::data_dir().join("agy-usage");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let out = crate::platform::timeout(35)
        .arg(&bin)
        .args(["-p", "/usage", "--output-format", "json", "--print-timeout", "30s"])
        .current_dir(&dir)
        .output()
        .map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("agy /usage: {e}: {}", String::from_utf8_lossy(&out.stderr).chars().take(200).collect::<String>()))?;
    agy_usage_shape(&v).ok_or_else(|| format!("agy /usage: no usage groups ({})", v["status"]))
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
        // omp's report first (when omp is there); each provider it lacks from its own harness
        let omp = fetch_usage();
        let omp_ok = omp.is_ok();
        let omp_err = omp.as_ref().err().cloned();
        let mut vendors = omp.map(|r| shape_usage(&r)).unwrap_or_default();
        let has = |vs: &Vec<Value>, f: &dyn Fn(&str) -> bool| vs.iter().any(|v| v["provider"].as_str().is_some_and(f));
        let sources: [(&str, &str, fn() -> Res<Value>, &dyn Fn(&str) -> bool); 3] = [
            ("antigravity", "agy", fetch_agy_usage, &|p: &str| p.contains("antigravity")),
            ("claude", "claude", fetch_claude_usage, &|p: &str| p == "anthropic"),
            ("codex", "codex", fetch_codex_usage, &|p: &str| p.starts_with("openai")),
        ];
        for (key, bin, fetch, covers) in sources {
            if has(&vendors, covers) || find_bin(bin).is_none() {
                continue;
            }
            match fetch() {
                Ok(v) => {
                    self.own.insert(key.to_string(), v.clone());
                    vendors.push(v);
                }
                Err(e) => {
                    eprintln!("adash: usage from {bin}: {e}");
                    if let Some(mut old) = self.own.get(key).cloned() {
                        old["stale"] = json!(true);
                        vendors.push(old);
                    }
                }
            }
        }
        let raw_ok: Res<()> = if omp_ok || !vendors.is_empty() { Ok(()) } else { Err(omp_err.unwrap_or_else(|| "no usage source".into())) };
        match raw_ok {
            Ok(()) => {
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
        assert_eq!(model_args("claude", Some("opus"), Some("low")).unwrap(), vec!["--model", "opus", "--effort", "low"]);
        assert_eq!(model_args("antigravity", Some("gemini-4-argon"), Some("high")).unwrap(), vec!["--model", "gemini-4-argon-high"]);
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

#[cfg(test)]
mod agy_usage_tests {
    #[test]
    fn agy_usage_groups_become_accounts() {
        let v: serde_json::Value = serde_json::json!({"command": {"name": "usage", "data": {"groups": [
            {"name": "Gemini Models", "buckets": [{"id": "gemini-weekly", "name": "Weekly Limit Remaining", "window": "weekly", "remaining_fraction": 0.94, "reset_time": "2026-11-14T17:24:17Z"},
                                                {"id": "gemini-5h", "name": "Five Hour Limit Remaining", "window": "5h", "remaining_fraction": 0.93}]},
            {"name": "Claude and GPT models", "buckets": [{"id": "3p-weekly", "window": "weekly", "remaining_fraction": 1}]}]}}});
        let s = super::agy_usage_shape(&v).unwrap();
        assert_eq!(s["provider"], "google-antigravity");
        assert_eq!(s["accounts"][0]["account"], "Gemini");
        let l = &s["accounts"][0]["limits"];
        assert_eq!(l[0]["windowLabel"], "7d");
        assert!((l[0]["usedFraction"].as_f64().unwrap() - 0.06).abs() < 1e-9);
        assert_eq!(l[0]["resetsAt"], 1794677057000i64);
        assert_eq!(s["accounts"][1]["account"], "Claude·GPT via Antigravity");
        assert!(super::agy_usage_shape(&serde_json::json!({"status": "SUCCESS"})).is_none());
    }

    #[test]
    fn claude_and_codex_usage_parse() {
        let t = "You are currently using your subscription\n\nCurrent week (all models): 34% used · resets Nov 14, 12pm (Asia/Shanghai)\nCurrent week (Fable): 24% used · resets Nov 14, 12pm (Asia/Shanghai)\nCurrent session: 5% used · resets 3pm\nsomething else";
        let v = super::claude_usage_shape(t).unwrap();
        let acc: Vec<&str> = v["accounts"].as_array().unwrap().iter().map(|a| a["account"].as_str().unwrap()).collect();
        assert_eq!(acc, vec!["all models", "Fable"]);
        let l = &v["accounts"][0]["limits"];
        assert_eq!((l[0]["windowLabel"].as_str(), l[0]["usedFraction"].as_f64()), (Some("7d"), Some(0.34)));
        assert_eq!(l[0]["resetsText"], "Nov 14, 12pm (Asia/Shanghai)");
        assert_eq!(l[1]["windowLabel"], "5h");
        assert!(super::claude_usage_shape("no usage here").is_none());
        let rl = serde_json::json!({"primary": {"used_percent": 48.0, "window_minutes": 300, "resets_at": 1793862443}, "secondary": {"used_percent": 12.5, "window_minutes": 10080}});
        let c = super::codex_usage_shape(&rl, crate::util::now_ms()).unwrap();
        let l = &c["accounts"][0]["limits"];
        assert_eq!((l[0]["windowLabel"].as_str(), l[0]["usedFraction"].as_f64(), l[0]["resetsAt"].as_i64()), (Some("5h"), Some(0.48), Some(1793862443000)));
        assert_eq!(l[1]["windowLabel"], "7d");
        assert_eq!(c["stale"], false);
        assert_eq!(super::codex_usage_shape(&rl, 0).unwrap()["stale"], true);
    }
}
