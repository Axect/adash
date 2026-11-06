//! `GET /api/agents/{name}/commands`: what the agent's own harness offers after `/` — its skills
//! (as that harness discovers them for the agent's cwd), custom/plugin commands, built-in
//! commands, plus adash templates. Insert forms were verified on throwaway sessions:
//! omp `/skill:<name>` (a plain `/<name>` reaches the model as text, no skill prompt).
use crate::util::LockExt;
use crate::util::{home, now_ms};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

struct Item {
    kind: &'static str,
    name: String,
    insert: String,
    /// form that also works in the middle of a message (verified per harness), if any
    mid: Option<String>,
    description: String,
    source: String,
    scope: &'static str,
}

impl Item {
    fn json(&self) -> Value {
        let mut v =
            json!({"kind": self.kind, "name": self.name, "insert": self.insert, "description": self.description, "source": self.source, "scope": self.scope});
        if let Some(m) = &self.mid {
            v["midInsert"] = json!(m);
        }
        v
    }
}

/// SKILL.md / command .md frontmatter: (name, description). Handles `key: value`, quoted
/// values and folded/literal block scalars.
pub fn frontmatter(text: &str) -> (Option<String>, Option<String>) {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }
    let body: Vec<&str> = lines.take_while(|l| l.trim() != "---").collect();
    let mut out: HashMap<String, String> = HashMap::new();
    let mut i = 0;
    while i < body.len() {
        let l = body[i];
        if let Some((k, v)) = l.split_once(':').filter(|(k, _)| !k.starts_with(' ') && !k.trim().is_empty()) {
            let v = v.trim();
            if v.is_empty() || v.starts_with('|') || v.starts_with('>') {
                let mut parts = vec![];
                while i + 1 < body.len() && (body[i + 1].starts_with(' ') || body[i + 1].trim().is_empty()) {
                    i += 1;
                    parts.push(body[i].trim());
                }
                out.insert(k.trim().into(), parts.join(" ").trim().to_string());
            } else {
                out.insert(k.trim().into(), v.trim_matches(['"', '\'']).to_string());
            }
        }
        i += 1;
    }
    (out.remove("name").filter(|s| !s.is_empty()), out.remove("description").filter(|s| !s.is_empty()))
}

fn short(s: &str) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::util::truncate(&one, 140)
}

/// Skills under `root/<dir>/SKILL.md` (one level; `.system`-style hidden dirs are scanned too
/// when `hidden` is set).
fn skills_in(root: &Path, scope: &'static str, source: &str, hidden: bool, out: &mut Vec<(String, String, PathBuf, &'static str, String)>) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for d in dirs {
        let base = d.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        if base.starts_with('.') {
            if hidden {
                skills_in(&d, "builtin", source, false, out);
            }
            continue;
        }
        let f = d.join("SKILL.md");
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        let (name, desc) = frontmatter(&text);
        out.push((name.unwrap_or(base), short(&desc.unwrap_or_default()), f, scope, source.to_string()));
    }
}

/// `*.md` command files in `dir` (name = file stem; nested dirs become `a:b`).
fn commands_in(dir: &Path, prefix: &str, scope: &'static str, source: &str, out: &mut Vec<(String, String, PathBuf, &'static str, String)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            let sub = p.file_name().unwrap().to_string_lossy().into_owned();
            commands_in(&p, &format!("{prefix}{sub}:"), scope, source, out);
        } else if p.extension().is_some_and(|x| x == "md") {
            let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            let (_, desc) = frontmatter(&text);
            let desc = desc.unwrap_or_else(|| {
                text.lines().find(|l| !l.trim().is_empty() && !l.starts_with("---")).unwrap_or("").trim_start_matches('#').trim().to_string()
            });
            out.push((format!("{prefix}{stem}"), short(&desc), p, scope, source.to_string()));
        }
    }
}

/// cwd, then each parent up to the repo root (a dir with .git) or $HOME (exclusive).
fn walk_up(cwd: &Path) -> Vec<PathBuf> {
    let home = home();
    let mut v = vec![];
    let mut d = Some(cwd.to_path_buf());
    while let Some(p) = d {
        if p == home || p.as_os_str().is_empty() {
            break;
        }
        v.push(p.clone());
        if p.join(".git").exists() {
            break;
        }
        d = p.parent().map(Path::to_path_buf);
    }
    v
}

fn glob_match(pat: &str, s: &str) -> bool {
    // `*` only (what skill include/ignore lists use)
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == s;
    }
    let mut rest = s;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            let Some(r) = rest.strip_prefix(part) else { return false };
            rest = r;
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else if let Some(at) = rest.find(part) {
            rest = &rest[at + part.len()..];
        } else {
            return false;
        }
    }
    true
}

/// omp settings that shape skill discovery, from ~/.omp/agent/config.yml and `--config` files.
#[derive(Default)]
struct OmpSkillCfg {
    disabled: bool,
    commands_off: bool,
    custom_dirs: Vec<PathBuf>,
    ignored: Vec<String>,
    include: Vec<String>,
    disabled_ext: HashSet<String>,
}

fn omp_cfg(args: &[String]) -> OmpSkillCfg {
    let mut files = vec![home().join(".omp/agent/config.yml")];
    for (i, a) in args.iter().enumerate() {
        if a == "--config" {
            if let Some(p) = args.get(i + 1) {
                files.push(PathBuf::from(p));
            }
        } else if let Some(p) = a.strip_prefix("--config=") {
            files.push(PathBuf::from(p));
        }
    }
    let mut c = OmpSkillCfg::default();
    for f in files {
        let Some(y) = std::fs::read_to_string(&f).ok().and_then(|s| serde_yaml::from_str::<serde_yaml::Value>(&s).ok()) else { continue };
        let sk = &y["skills"];
        if sk["enabled"].as_bool() == Some(false) {
            c.disabled = true;
        }
        if sk["enableSkillCommands"].as_bool() == Some(false) {
            c.commands_off = true;
        }
        let strs =
            |v: &serde_yaml::Value| v.as_sequence().map(|s| s.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>()).unwrap_or_default();
        c.custom_dirs.extend(strs(&sk["customDirectories"]).into_iter().map(|p| PathBuf::from(p.replacen('~', &home().to_string_lossy(), 1))));
        c.ignored.extend(strs(&sk["ignoredSkills"]));
        c.include.extend(strs(&sk["includeSkills"]));
        c.disabled_ext.extend(strs(&y["disabledExtensions"]));
    }
    if args.iter().any(|a| a == "--no-skills") {
        c.disabled = true;
    }
    if let Some(list) = crate::session::arg_value(args, "--skills") {
        c.include = list.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    }
    c
}

const OMP_BUILTINS: &[(&str, &str)] = &[
    ("compact", "컨텍스트 압축"),
    ("clear", "컨텍스트 비우기(같은 세션)"),
    ("new", "새 세션"),
    ("resume", "이전 세션 이어 하기"),
    ("model", "모델 바꾸기"),
    ("handoff", "새 세션으로 넘기기"),
    ("context", "컨텍스트 사용량"),
    ("usage", "사용량·한도"),
    ("todo", "할 일 목록"),
    ("jobs", "백그라운드 작업"),
    ("tree", "세션 트리"),
    ("branch", "분기"),
    ("fork", "포크"),
    ("rename", "세션 이름"),
    ("plan", "계획 모드"),
    ("goal", "목표 모드"),
    ("loop", "반복"),
    ("btw", "곁가지 질문"),
    ("retry", "다시 시도"),
    ("memory", "메모리"),
    ("settings", "설정"),
    ("tools", "도구 목록"),
    ("extensions", "확장 관리"),
    ("plugins", "플러그인"),
    ("mcp", "MCP"),
    ("export", "내보내기"),
    ("copy", "마지막 답 복사"),
    ("hotkeys", "단축키"),
    ("changelog", "변경 내역"),
    ("shake", "무거운 내용 덜어 내기"),
    ("drop", "세션 삭제 후 새로"),
    ("fast", "빠른 모드"),
    ("vibe", "vibe 모드"),
    ("exit", "종료"),
];
const AGY_BUILTINS: &[(&str, &str)] = &[
    ("plan", "계획 초안"),
    ("goal", "완료까지 실행"),
    ("clear", "대화 비우기"),
    ("model", "모델 선택"),
    ("effort", "추론 강도"),
    ("context", "컨텍스트 사용량"),
    ("resume", "대화 고르기"),
    ("rewind", "되돌리기"),
    ("skills", "스킬 목록 (reload: 다시 읽기)"),
    ("plugin", "플러그인 관리"),
    ("mcp", "MCP 서버"),
    ("btw", "곁가지 질문"),
    ("learn", "교정 내용을 규칙·스킬로 저장"),
    ("diff", "변경 보기"),
    ("copy", "답 복사"),
    ("rename", "이름 바꾸기"),
    ("usage", "할당량"),
    ("config", "설정"),
    ("permissions", "도구 권한"),
    ("schedule", "예약 실행"),
    ("tasks", "백그라운드 셸"),
    ("help", "도움말"),
    ("exit", "종료"),
];
const CLAUDE_BUILTINS: &[(&str, &str)] = &[
    ("compact", "대화 요약으로 컨텍스트 확보"),
    ("clear", "새 빈 대화"),
    ("model", "모델 바꾸기"),
    ("resume", "대화 이어 하기"),
    ("context", "컨텍스트 사용량"),
    ("cost", "사용량"),
    ("memory", "CLAUDE.md 편집"),
    ("plan", "계획 모드"),
    ("goal", "조건까지 계속"),
    ("btw", "곁가지 질문"),
    ("rewind", "되돌리기"),
    ("agents", "서브에이전트"),
    ("skills", "스킬 목록"),
    ("plugin", "플러그인"),
    ("mcp", "MCP"),
    ("permissions", "권한"),
    ("config", "설정"),
    ("review", "변경 검토"),
    ("init", "CLAUDE.md 만들기"),
    ("export", "내보내기"),
    ("help", "도움말"),
    ("exit", "종료"),
];
const CODEX_BUILTINS: &[(&str, &str)] = &[
    ("model", "모델·추론 강도"),
    ("compact", "요약으로 컨텍스트 확보"),
    ("new", "새 대화"),
    ("resume", "저장된 대화"),
    ("review", "변경 검토"),
    ("diff", "git diff"),
    ("status", "세션 설정·토큰"),
    ("skills", "스킬"),
    ("plan", "계획 모드"),
    ("goal", "목표"),
    ("permissions", "권한"),
    ("init", "AGENTS.md 만들기"),
    ("mention", "파일 언급"),
    ("fork", "포크"),
    ("rename", "이름 바꾸기"),
    ("copy", "답 복사"),
    ("export", "내보내기"),
];

type Found = Vec<(String, String, PathBuf, &'static str, String)>;

fn scope_of(p: &Path, cwd: &Path) -> &'static str {
    if p.starts_with(cwd)
        || walk_up(cwd).iter().any(|d| {
            p.starts_with(d.join(".omp"))
                || p.starts_with(d.join(".claude"))
                || p.starts_with(d.join(".agents"))
                || p.starts_with(d.join(".agent"))
                || p.starts_with(d.join(".codex"))
        })
    {
        "project"
    } else {
        "user"
    }
}

/// Skills + commands for one harness and cwd (not cached).
fn discover(harness: &str, cwd: &Path, args: &[String]) -> (Vec<Item>, Vec<PathBuf>) {
    let h = home();
    let up = walk_up(cwd);
    let mut skills: Found = vec![];
    let mut cmds: Found = vec![];
    let mut roots: Vec<PathBuf> = vec![];
    let sk = |root: PathBuf, scope: &'static str, src: &str, hidden: bool, skills: &mut Found, roots: &mut Vec<PathBuf>| {
        skills_in(&root, scope, src, hidden, skills);
        roots.push(root);
    };
    let mut items = vec![];
    match harness {
        "omp" => {
            let cfg = omp_cfg(args);
            if !cfg.disabled {
                // provider priority (first name wins): native, claude, agents, codex, opencode
                for d in &up {
                    sk(d.join(".omp/skills"), "project", "omp", false, &mut skills, &mut roots);
                }
                sk(h.join(".omp/agent/skills"), "user", "omp", false, &mut skills, &mut roots);
                sk(h.join(".claude/skills"), "user", "claude", false, &mut skills, &mut roots);
                for d in &up {
                    sk(d.join(".claude/skills"), "project", "claude", false, &mut skills, &mut roots);
                }
                for d in &up {
                    sk(d.join(".agent/skills"), "project", "agents", false, &mut skills, &mut roots);
                    sk(d.join(".agents/skills"), "project", "agents", false, &mut skills, &mut roots);
                }
                sk(h.join(".agent/skills"), "user", "agents", false, &mut skills, &mut roots);
                sk(h.join(".agents/skills"), "user", "agents", false, &mut skills, &mut roots);
                sk(h.join(".codex/skills"), "user", "codex", false, &mut skills, &mut roots);
                for d in &up {
                    sk(d.join(".codex/skills"), "project", "codex", false, &mut skills, &mut roots);
                }
                sk(h.join(".config/opencode/skills"), "user", "opencode", false, &mut skills, &mut roots);
                // custom directories replace same-named default skills; managed skills come last
                let mut custom: Found = vec![];
                for d in &cfg.custom_dirs {
                    sk(d.clone(), "user", "custom", false, &mut custom, &mut roots);
                }
                let custom_names: HashSet<String> = custom.iter().map(|c| c.0.clone()).collect();
                skills.retain(|s| !custom_names.contains(&s.0));
                skills.extend(custom);
                sk(h.join(".omp/agent/managed-skills"), "user", "managed", false, &mut skills, &mut roots);
                let mut seen = HashSet::new();
                skills.retain(|s| seen.insert(s.0.clone()));
                skills.retain(|s| {
                    !cfg.disabled_ext.contains(&format!("skill:{}", s.0))
                        && !cfg.ignored.iter().any(|g| glob_match(g, &s.0))
                        && (cfg.include.is_empty() || cfg.include.iter().any(|g| glob_match(g, &s.0)))
                });
                if !cfg.commands_off {
                    for s in &skills {
                        // omp expands `/skill:<name>` anywhere in the message (MID_PROMPT_SKILL_RE; verified)
                        items.push(Item {
                            kind: "skill",
                            name: s.0.clone(),
                            insert: format!("/skill:{}", s.0),
                            mid: Some(format!("/skill:{}", s.0)),
                            description: s.1.clone(),
                            source: s.4.clone(),
                            scope: s.3,
                        });
                    }
                }
            }
            for (dir, scope) in [(h.join(".omp/agent/commands"), "user"), (h.join(".codex/commands"), "user"), (h.join(".claude/commands"), "user")] {
                commands_in(&dir, "", scope, "omp", &mut cmds);
                roots.push(dir);
            }
            for d in &up {
                for sub in [".omp/commands", ".claude/commands", ".agents/commands", ".agent/commands", ".codex/commands"] {
                    commands_in(&d.join(sub), "", "project", "omp", &mut cmds);
                    roots.push(d.join(sub));
                }
            }
            installed_builtins(&mut items, "omp", OMP_BUILTINS);
        }
        "antigravity" => {
            for d in &up {
                for sub in [".agents/skills", ".agent/skills", "_agents/skills", "_agent/skills"] {
                    sk(d.join(sub), "project", "workspace", false, &mut skills, &mut roots);
                }
            }
            sk(h.join(".gemini/antigravity-cli/skills"), "user", "agy", false, &mut skills, &mut roots);
            sk(h.join(".gemini/config/skills"), "user", "agy", false, &mut skills, &mut roots);
            if let Ok(rd) = std::fs::read_dir(h.join(".gemini/antigravity-cli/plugins")) {
                for p in rd.flatten() {
                    sk(p.path().join("skills"), "user", &format!("plugin:{}", p.file_name().to_string_lossy()), false, &mut skills, &mut roots);
                }
            }
            sk(h.join(".gemini/antigravity-cli/builtin/skills"), "builtin", "agy", false, &mut skills, &mut roots);
            let mut seen = HashSet::new();
            skills.retain(|s| seen.insert(s.0.clone()));
            // agy expands `/<skill>` only at the start and does not inline `@file` (probed
            // 2026-10-30); mid-message the agent gets a readable reference to the SKILL.md
            for s in &skills {
                items.push(Item {
                    kind: "skill",
                    name: s.0.clone(),
                    insert: format!("/{}", s.0),
                    mid: Some(format!("{} 스킬({})", s.0, tilde(&s.2))),
                    description: s.1.clone(),
                    source: s.4.clone(),
                    scope: s.3,
                });
            }
            builtins(&mut items, AGY_BUILTINS, "/");
        }
        "claude" => {
            sk(h.join(".claude/skills"), "user", "claude", false, &mut skills, &mut roots);
            for d in &up {
                sk(d.join(".claude/skills"), "project", "claude", false, &mut skills, &mut roots);
            }
            commands_in(&h.join(".claude/commands"), "", "user", "claude", &mut cmds);
            for d in &up {
                commands_in(&d.join(".claude/commands"), "", "project", "claude", &mut cmds);
            }
            // enabled plugins: /<plugin>:<skill|command>
            let enabled: Vec<String> = std::fs::read_to_string(h.join(".claude/settings.json"))
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .and_then(|v| v["enabledPlugins"].as_object().map(|m| m.iter().filter(|(_, on)| on.as_bool() == Some(true)).map(|(k, _)| k.clone()).collect()))
                .unwrap_or_default();
            for key in enabled {
                let (plugin, market) = key.split_once('@').unwrap_or((key.as_str(), ""));
                let base = h.join(".claude/plugins/cache").join(market).join(plugin);
                // newest version directory
                let Some(ver) = std::fs::read_dir(&base).ok().and_then(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).max()) else { continue };
                let mut ps: Found = vec![];
                skills_in(&ver.join("skills"), "user", plugin, false, &mut ps);
                commands_in(&ver.join("commands"), "", "user", plugin, &mut ps);
                roots.push(ver.clone());
                for (n, d, p, sc, src) in ps {
                    cmds.push((format!("{plugin}:{n}"), d, p, sc, src));
                }
            }
            let mut seen = HashSet::new();
            skills.retain(|s| seen.insert(s.0.clone()));
            for s in &skills {
                // `/name` only expands at the start; an @-mention of the file inlines it anywhere (verified)
                items.push(Item {
                    kind: "skill",
                    name: s.0.clone(),
                    insert: format!("/{}", s.0),
                    mid: Some(format!("@{}", tilde(&s.2))),
                    description: s.1.clone(),
                    source: s.4.clone(),
                    scope: s.3,
                });
            }
            installed_builtins(&mut items, "claude", CLAUDE_BUILTINS);
        }
        "codex" => {
            sk(h.join(".codex/skills"), "user", "codex", true, &mut skills, &mut roots);
            sk(h.join(".agents/skills"), "user", "agents", false, &mut skills, &mut roots);
            for d in &up {
                sk(d.join(".agents/skills"), "project", "agents", false, &mut skills, &mut roots);
            }
            let mut seen = HashSet::new();
            skills.retain(|s| seen.insert(s.0.clone()));
            for s in &skills {
                // codex injects `$name` skills wherever they appear (verified)
                items.push(Item {
                    kind: "skill",
                    name: s.0.clone(),
                    insert: format!("${}", s.0),
                    mid: Some(format!("${}", s.0)),
                    description: s.1.clone(),
                    source: s.4.clone(),
                    scope: s.3,
                });
            }
            let mut prompts: Found = vec![];
            commands_in(&h.join(".codex/prompts"), "", "user", "codex", &mut prompts);
            roots.push(h.join(".codex/prompts"));
            for (n, d, _, sc, src) in prompts {
                items.push(Item {
                    kind: "command",
                    name: format!("prompts:{n}"),
                    insert: format!("/prompts:{n}"),
                    mid: None,
                    description: d,
                    source: src,
                    scope: sc,
                });
            }
            builtins(&mut items, CODEX_BUILTINS, "/");
        }
        _ => {}
    }
    let mut seen = HashSet::new();
    for (n, d, p, sc, src) in cmds {
        if seen.insert(n.clone()) {
            let scope = if sc == "project" { scope_of(&p, cwd) } else { sc };
            // claude: an @-mention of the command file works mid-message; others start-only
            let mid = (harness == "claude").then(|| format!("@{}", tilde(&p)));
            items.push(Item { kind: "command", insert: format!("/{n}"), mid, name: n, description: d, source: src, scope });
        }
    }
    (items, roots)
}

fn tilde(p: &Path) -> String {
    let h = home();
    p.strip_prefix(&h).map(|r| format!("~/{}", r.display())).unwrap_or_else(|_| p.display().to_string())
}

/// Built-in commands as the installed harness defines them (Claude Code, omp), else the hand list.
/// Hand-list entries keep their Korean description; the rest use the harness's own (English) text.
fn installed_builtins(items: &mut Vec<Item>, kind: &str, fallback: &[(&str, &str)]) {
    let Some(found) = extracted(kind).filter(|v| v.len() >= 10) else { return builtins(items, fallback, "/") };
    for (n, d) in found {
        let ko = fallback.iter().find(|(f, _)| *f == n).map(|(_, d)| d.to_string());
        items.push(Item {
            kind: "command",
            insert: format!("/{n}"),
            name: n,
            mid: None,
            description: ko.unwrap_or(d),
            source: "builtin".into(),
            scope: "builtin",
        });
    }
}

/// (name, description) of the installed binary's built-ins, cached per binary path + mtime.
fn extracted(kind: &str) -> Option<Vec<(String, String)>> {
    static CACHE: LazyLock<Mutex<HashMap<String, (PathBuf, u128, Vec<(String, String)>)>>> = LazyLock::new(Default::default);
    let bin = std::fs::canonicalize(crate::launch::find_bin(kind)?).ok()?;
    let mt = std::fs::metadata(&bin).and_then(|m| m.modified()).ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    if let Some((p, m, v)) = CACHE.plock().get(kind) {
        if *p == bin && *m == mt {
            return Some(v.clone());
        }
    }
    let bytes = std::fs::read(&bin).ok()?;
    let v = match kind {
        "claude" => claude_builtins(&bytes),
        "omp" => omp_builtins(&bytes),
        _ => return None,
    };
    CACHE.plock().insert(kind.into(), (bin, mt, v.clone()));
    Some(v)
}

/// Claude Code command objects: `type:"local|local-jsx|prompt",name:"x",description:"…"`, then
/// optional `argumentHint`; `isHidden:!0` / `isEnabled:()=>!1` ones are skipped.
fn claude_builtins(b: &[u8]) -> Vec<(String, String)> {
    static RE: LazyLock<regex::bytes::Regex> = LazyLock::new(|| {
        regex::bytes::Regex::new(r#"type:"(?:local|local-jsx|prompt)",name:"([a-z][a-z0-9-]*)",description:"((?:[^"\\]|\\.){1,200})""#).unwrap()
    });
    static HINT: LazyLock<regex::bytes::Regex> = LazyLock::new(|| regex::bytes::Regex::new(r#"^,argumentHint:"([^"]{1,60})""#).unwrap());
    let mut out: Vec<(String, String)> = vec![];
    for c in RE.captures_iter(b) {
        let end = c.get(0).unwrap().end();
        let tail = &b[end..(end + 600).min(b.len())];
        let tail = &tail[..tail.windows(5).position(|w| w == b"load:").unwrap_or(tail.len())];
        let has = |s: &[u8]| tail.windows(s.len()).any(|w| w == s);
        if has(b"isHidden:!0") || has(b"isEnabled:()=>!1") {
            continue;
        }
        let name = String::from_utf8_lossy(&c[1]).into_owned();
        let mut d = String::from_utf8_lossy(&c[2]).replace("\\\"", "\"");
        if let Some(h) = HINT.captures(tail) {
            d = format!("{d} · {}", String::from_utf8_lossy(&h[1]));
        }
        if !out.iter().any(|(n, _)| *n == name) {
            out.push((name, d));
        }
    }
    out.sort();
    out
}

/// omp slash commands: the pretty-printed table entries `{ name: "x", icon: …, description: "…" }`.
fn omp_builtins(b: &[u8]) -> Vec<(String, String)> {
    static RE: LazyLock<regex::bytes::Regex> =
        LazyLock::new(|| regex::bytes::Regex::new(r#"\n  \{\n    name: "([a-z][a-z0-9-]*)",\n((?:    [^\n]*\n){0,6})"#).unwrap());
    static DESC: LazyLock<regex::bytes::Regex> = LazyLock::new(|| regex::bytes::Regex::new(r#"    description: "([^"\n]*)""#).unwrap());
    static ALIASES: LazyLock<regex::bytes::Regex> = LazyLock::new(|| regex::bytes::Regex::new(r#"    aliases: \[([^\]\n]*)\]"#).unwrap());
    let mut out: Vec<(String, String)> = vec![];
    let mut aliases: Vec<(String, String)> = vec![];
    for c in RE.captures_iter(b) {
        let body = &c[2];
        if !body.windows(6).any(|w| w == b"icon: ") || body.windows(7).any(|w| w == b"hidden:") {
            continue;
        }
        let Some(d) = DESC.captures(body) else { continue };
        let name = String::from_utf8_lossy(&c[1]).into_owned();
        let desc = String::from_utf8_lossy(&d[1]).into_owned();
        if !out.iter().any(|(n, _)| *n == name) {
            out.push((name.clone(), desc.clone()));
        }
        // `aliases: ["a", "b"]`: the same command under another name
        if let Some(al) = ALIASES.captures(body) {
            for x in String::from_utf8_lossy(&al[1]).split(',').map(|x| x.trim().trim_matches('"').trim_end_matches(':').to_string()) {
                if !x.is_empty() && !out.iter().any(|(n, _)| *n == x) {
                    aliases.push((x, format!("= /{name} · {desc}")));
                }
            }
        }
    }
    // a real command of that name wins over an alias
    for (x, d) in aliases {
        if !out.iter().any(|(n, _)| *n == x) {
            out.push((x, d));
        }
    }
    out.sort();
    out
}

fn builtins(items: &mut Vec<Item>, list: &[(&str, &str)], prefix: &str) {
    for (n, d) in list {
        items.push(Item {
            kind: "command",
            name: (*n).into(),
            insert: format!("{prefix}{n}"),
            mid: None,
            description: (*d).into(),
            source: "builtin".into(),
            scope: "builtin",
        });
    }
}

/// Newest mtime among the scanned roots and their direct children (skill dirs).
fn signature(roots: &[PathBuf]) -> u128 {
    let mt = |p: &Path| {
        std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0)
    };
    let mut s = 0u128;
    for r in roots {
        s = s.max(mt(r));
        if let Ok(rd) = std::fs::read_dir(r) {
            for e in rd.flatten() {
                s = s.max(mt(&e.path()));
            }
        }
    }
    s
}

struct Cached {
    at: i64,
    sig: u128,
    roots: Vec<PathBuf>,
    items: Vec<Value>,
}

static CACHE: LazyLock<Mutex<HashMap<(String, String, String), Cached>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Catalog for an agent: harness discovery for its cwd (and its process flags), cached 60 s per
/// (harness, cwd, flags) and refreshed early when a scanned directory changes; plus templates.
/// Input prefixes a harness treats as something other than a message to the model.
/// omp (checked in its input controller): "!" / "!!" run a shell command (`!!` keeps it out of
/// the context), "$ " / "$$ " run Python. claude: "!" bash mode, "#" memory. codex: "!"
/// shell. "/" (slash commands) is left out on purpose: the dashboard inserts those itself.
pub fn prefixes(agent_type: &str) -> Vec<Value> {
    let h = agent_type;
    crate::harness::get(agent_type)
        .map(|x| x.input_prefixes())
        .unwrap_or_default()
        .into_iter()
        .map(|(prefix, kind)| {
            let warning = match kind {
                "shell" => format!("{} 로 시작하면 {h} 가 셸 명령으로 실행합니다", prefix.trim()),
                "python" => format!("'{prefix}' 로 시작하면 {h} 가 Python 코드로 실행합니다"),
                "memory" => format!("{} 로 시작하면 {h} 가 메모리에 저장합니다", prefix.trim()),
                _ => format!("{} 로 시작하면 {h} 가 특수 입력으로 처리합니다", prefix.trim()),
            };
            json!({"prefix": prefix, "kind": kind, "warning": warning})
        })
        .collect()
}

/// Text that starts with one of the harness's prefixes, sent as a plain message: a zero-width
/// space in front (verified with omp: " !x" still runs the shell, "\u{200b}!x" arrives as text).
pub fn as_text(agent_type: &str, text: &str) -> String {
    let t = text.trim_start();
    let hit = prefixes(agent_type).iter().any(|p| {
        let pre = p["prefix"].as_str().unwrap_or("");
        // "$ " also matches "$$ " and "$\t"
        if pre == "$ " {
            t.starts_with('$') && t.trim_start_matches('$').starts_with(char::is_whitespace)
        } else {
            t.starts_with(pre)
        }
    });
    if hit {
        format!("\u{200b}{t}")
    } else {
        text.to_string()
    }
}

pub fn catalog(a: &crate::collect::Agent) -> Value {
    let procs = a.handle.as_deref().and_then(|h| crate::session::scan_procs().remove(h)).unwrap_or_default();
    let p = procs.iter().rev().find(|p| p.kind == a.agent_type).or(procs.last());
    let args: Vec<String> = p.map(|p| p.args.clone()).unwrap_or_default();
    let cwd = p.map(|p| p.cwd.clone()).filter(|c| !c.as_os_str().is_empty()).unwrap_or_else(|| PathBuf::from(&a.worktree_path));
    let key = (
        a.agent_type.clone(),
        cwd.to_string_lossy().into_owned(),
        args.iter().filter(|x| x.starts_with("--skills") || x.starts_with("--no-skills") || x.starts_with("--config")).cloned().collect::<Vec<_>>().join(" "),
    );
    let now = now_ms();
    let mut cache = CACHE.plock();
    let fresh = cache.get(&key).is_some_and(|c| now - c.at < 60_000 && signature(&c.roots) == c.sig);
    if !fresh {
        let (items, roots) = discover(&a.agent_type, &cwd, &args);
        let sig = signature(&roots);
        cache.insert(key.clone(), Cached { at: now, sig, roots, items: items.iter().map(Item::json).collect() });
    }
    let c = &cache[&key];
    let items = c.items.clone();
    json!({"harness": a.agent_type, "cwd": key.1, "items": items, "prefixes": prefixes(&a.agent_type), "fetchedAt": c.at})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_reads_plain_quoted_and_folded_descriptions() {
        assert_eq!(
            frontmatter("---\nname: magi-research\ndescription: \"Run research with three agents\"\n---\nbody"),
            (Some("magi-research".into()), Some("Run research with three agents".into()))
        );
        assert_eq!(
            frontmatter("---\nname: x\ndescription: >\n  first line\n  second line\nallowed-tools: Read\n---\n").1.as_deref(),
            Some("first line second line")
        );
        assert_eq!(frontmatter("# no frontmatter"), (None, None));
        assert!(glob_match("magi-*", "magi-research") && !glob_match("magi-*", "academic-cv") && glob_match("*-cv", "academic-cv"));
    }

    #[test]
    fn omp_skills_dedupe_by_priority_and_insert_as_skill_commands() {
        let dir = std::env::temp_dir().join(format!("adash-cmd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let proj = dir.join("repo");
        std::fs::create_dir_all(proj.join(".git")).unwrap();
        for (root, name, desc) in
            [(".omp/skills", "shared", "project version"), (".claude/skills", "shared", "claude version"), (".agents/skills", "only-agents", "agents skill")]
        {
            let d = proj.join(root).join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("SKILL.md"), format!("---\nname: {name}\ndescription: {desc}\n---\n")).unwrap();
        }
        std::fs::create_dir_all(proj.join(".omp/commands")).unwrap();
        std::fs::write(proj.join(".omp/commands/deploy.md"), "Deploy the site\n").unwrap();
        let (items, _) = discover("omp", &proj, &[]);
        let shared: Vec<&Item> = items.iter().filter(|i| i.name == "shared").collect();
        assert_eq!(shared.len(), 1);
        assert_eq!((shared[0].insert.as_str(), shared[0].description.as_str(), shared[0].scope), ("/skill:shared", "project version", "project"));
        assert!(items.iter().any(|i| i.name == "only-agents" && i.kind == "skill"));
        assert!(items.iter().any(|i| i.name == "deploy" && i.kind == "command" && i.insert == "/deploy" && i.description == "Deploy the site"));
        assert!(items.iter().any(|i| i.name == "compact" && i.scope == "builtin"));
        // --no-skills hides skills, keeps commands
        let (items, _) = discover("omp", &proj, &["omp".into(), "--no-skills".into()]);
        assert!(!items.iter().any(|i| i.kind == "skill") && items.iter().any(|i| i.name == "deploy"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod prefix_tests {
    #[test]
    fn as_text_guards_only_harness_prefixes() {
        assert_eq!(super::as_text("omp", "![fig](plots/a.png) 설명"), "\u{200b}![fig](plots/a.png) 설명");
        assert_eq!(super::as_text("omp", "$ print(1)"), "\u{200b}$ print(1)");
        assert_eq!(super::as_text("omp", "$5 비용"), "$5 비용", "$ needs whitespace after it");
        assert_eq!(super::as_text("omp", "# 제목"), "# 제목", "# is plain text in omp");
        assert_eq!(super::as_text("claude", "# 메모"), "\u{200b}# 메모");
        assert_eq!(super::as_text("antigravity", "!x"), "!x");
    }

    #[test]
    fn builtins_come_from_the_installed_harness() {
        let claude = br#"x={type:"local",name:"reload-plugins",description:"Activate pending plugin changes in the current session",argumentHint:"[--force]",load:()=>1};y={type:"local-jsx",name:"secret",description:"hidden one",isHidden:!0,load:()=>1};z={type:"prompt",name:"off",description:"d",isEnabled:()=>!1,load:1}"#;
        assert_eq!(
            super::claude_builtins(claude),
            vec![("reload-plugins".to_string(), "Activate pending plugin changes in the current session · [--force]".to_string())]
        );
        let omp = b"\n  {\n    name: \"compact\",\n    icon: \"compress\",\n    description: \"Manually compact the session context\",\n  },\n  {\n    name: \"soft\",\n    description: \"subcommand\",\n  },";
        assert_eq!(super::omp_builtins(omp), vec![("compact".to_string(), "Manually compact the session context".to_string())]);
        // the real binaries, when installed here
        if let Some(v) = super::extracted("claude") {
            assert!(v.iter().any(|(n, _)| n == "reload-plugins"), "{} claude built-ins", v.len());
        }
        if let Some(v) = super::extracted("omp") {
            assert!(v.iter().any(|(n, _)| n == "compact") && v.iter().any(|(n, _)| n == "reload-plugins") && v.len() >= 30, "{} omp built-ins", v.len());
            let mut names: Vec<&String> = v.iter().map(|(n, _)| n).collect();
            names.sort();
            names.dedup();
            assert_eq!(names.len(), v.len(), "no duplicates");
        }
    }
}
