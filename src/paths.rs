//! Path resolution for paths agents mention in chat (`/file`, `/open`, `/stat`).
//! Order: absolute/`~`/`file://` → worktree → session cwd → directories of recent tool-call
//! paths → suffix search in a cached worktree file index. Access rules apply to every result:
//! under the worktree or $HOME, no `..`, no credential locations.
use crate::util::home;
use crate::util::LockExt;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const INDEX_TTL: Duration = Duration::from_secs(60);
const INDEX_CAP: usize = 200_000;
const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", ".venv"];
const MAX_CANDIDATES: usize = 20;

#[derive(Debug, Clone)]
pub struct Resolved {
    pub path: PathBuf,
    pub ambiguous: bool,
    pub candidates: Vec<String>,
}

/// Relative file list per worktree root, rebuilt at most once per minute.
#[derive(Default)]
pub struct FileIndex {
    roots: HashMap<PathBuf, (Instant, Arc<Vec<PathBuf>>)>,
    /// git repos: files git does not ignore (tracked + untracked), per repo top level
    git: HashMap<PathBuf, (Instant, Option<Arc<Vec<PathBuf>>>)>,
}

impl FileIndex {
    pub fn files(&mut self, root: &Path) -> Arc<Vec<PathBuf>> {
        if let Some((at, v)) = self.roots.get(root) {
            if at.elapsed() < INDEX_TTL {
                return v.clone();
            }
        }
        let v = Arc::new(walk(root));
        self.roots.insert(root.to_path_buf(), (Instant::now(), v.clone()));
        v
    }

    /// `git ls-files -co --exclude-standard` relative to the repo top level (None: not a repo).
    pub fn git_files(&mut self, top: &Path) -> Option<Arc<Vec<PathBuf>>> {
        if let Some((at, v)) = self.git.get(top) {
            if at.elapsed() < INDEX_TTL {
                return v.clone();
            }
        }
        let out = crate::platform::timeout(10)
            .args(["git", "-C"])
            .arg(top)
            .args(["ls-files", "-co", "--exclude-standard", "-z"])
            .output()
            .ok()
            .filter(|o| o.status.success());
        let v = out.map(|o| {
            Arc::new(
                o.stdout
                    .split(|b| *b == 0)
                    .filter(|s| !s.is_empty())
                    .take(INDEX_CAP)
                    .map(|s| PathBuf::from(String::from_utf8_lossy(s).into_owned()))
                    .filter(|p| !p.components().any(|c| SKIP_DIRS.iter().any(|d| c.as_os_str() == *d)))
                    .collect::<Vec<_>>(),
            )
        });
        self.git.insert(top.to_path_buf(), (Instant::now(), v.clone()));
        v
    }
}

/// Repo top level containing `p` (walks up to a `.git`).
fn git_top(p: &Path) -> Option<PathBuf> {
    p.ancestors().find(|a| a.join(".git").exists()).map(Path::to_path_buf)
}

/// One completion: path as the user would type it (relative to the base, or as typed: `~/…`,
/// `/…`, `../…`), dirs end with "/".
#[derive(Debug, Clone, serde::Serialize)]
pub struct Completion {
    pub path: String,
    pub kind: &'static str,
    pub size: Option<u64>,
    pub mtime: i64,
    #[serde(skip)]
    pub score: i64,
    /// text to put in the prompt for this harness (`mention`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insert: Option<String>,
}

/// The prompt text referencing `path` in this harness: omp/claude `@path` (`@"a b"` with
/// spaces), others the plain path (quoted with spaces).
pub fn mention(agent_type: &str, path: &str) -> String {
    crate::harness::get(agent_type).map(|h| h.mention(path)).unwrap_or_else(|| path.to_string())
}

/// Case-insensitive fuzzy score of `name` for `q` (0 = no match).
fn fuzzy(name: &str, q: &str) -> i64 {
    if q.is_empty() {
        return 1;
    }
    let n = name.to_lowercase();
    let q = q.to_lowercase();
    if n == q {
        return 1000;
    }
    if n.starts_with(&q) {
        return 800;
    }
    if n.contains(&q) {
        return 450;
    }
    let mut it = n.chars();
    if q.chars().all(|c| it.any(|x| x == c)) {
        return 200;
    }
    0
}

/// `@` completion for a query typed after "@": with a "/" (or `~`, `/`, `..`) the directory
/// part is listed and filtered by the last component; a bare word is fuzzy-matched against the
/// repo's non-ignored files (`all`: every file, ignore rules off). Same access rules as /stat.
pub fn complete(index: &Mutex<FileIndex>, worktree: &Path, base: &Path, q: &str, limit: usize, all: bool) -> Result<Vec<Completion>, (u16, String)> {
    let roots: Vec<PathBuf> = [worktree, home().as_path()].iter().filter_map(|r| r.canonicalize().ok()).collect();
    let allowed = |c: &Path| roots.iter().any(|r| c.starts_with(r)) && !denied(c);
    let limit = limit.clamp(1, 200);
    let q = q.trim_start_matches('"');
    let meta = |p: &Path| std::fs::metadata(p).ok();
    let mut out: Vec<Completion> = Vec::new();
    if q.contains('/') || q.starts_with('~') {
        let (dir_part, prefix) = match q.rfind('/') {
            Some(i) => (&q[..=i], &q[i + 1..]),
            None => ("~/", ""),
        };
        let dir = expand(dir_part.trim_end_matches('/').trim_end_matches('/'), base);
        let dir = if dir_part == "/" { PathBuf::from("/") } else { dir };
        let canon = dir.canonicalize().map_err(|_| (404, format!("not found: {dir_part}")))?;
        if !allowed(&canon) {
            return Err((403, "denied: outside worktree and $HOME".into()));
        }
        // inside a repo (and not `all`): only names git does not ignore
        let keep: Option<HashSet<PathBuf>> = if all {
            None
        } else {
            git_top(&canon).and_then(|top| {
                let files = index.plock().git_files(&top)?;
                let rel = canon.strip_prefix(&top).ok()?.to_path_buf();
                Some(
                    files
                        .iter()
                        .filter_map(|f| f.strip_prefix(&rel).ok())
                        .filter_map(|r| r.components().next())
                        .map(|c| PathBuf::from(c.as_os_str()))
                        .collect(),
                )
            })
        };
        let Ok(rd) = std::fs::read_dir(&canon) else { return Ok(out) };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if SKIP_DIRS.contains(&name.as_str()) || (name.starts_with('.') && !prefix.starts_with('.')) {
                continue;
            }
            if keep.as_ref().is_some_and(|k| !k.contains(Path::new(&name))) {
                continue;
            }
            let s = fuzzy(&name, prefix);
            if s == 0 || denied(&e.path()) {
                continue;
            }
            let Some(m) = meta(&e.path()) else { continue };
            let dir = m.is_dir();
            out.push(Completion {
                path: format!("{dir_part}{name}{}", if dir { "/" } else { "" }),
                kind: if dir { "dir" } else { "file" },
                size: (!dir).then_some(m.len()),
                mtime: crate::util::mtime_ms(&e.path()),
                score: s + if dir { 5 } else { 0 },
                insert: None,
            });
        }
    } else {
        let canon = base.canonicalize().map_err(|_| (404, "base directory not found".to_string()))?;
        let (top, files) = match (all, git_top(&canon)) {
            (false, Some(top)) => match index.plock().git_files(&top) {
                Some(f) => (top, f),
                None => (canon.clone(), index.plock().files(&canon)),
            },
            _ => (canon.clone(), index.plock().files(&canon)),
        };
        let rel_base = canon.strip_prefix(&top).map(Path::to_path_buf).unwrap_or_default();
        let mut dirs: HashSet<PathBuf> = HashSet::new();
        for f in files.iter() {
            // under the base only (the agent's cwd); dirs from file ancestors
            let Ok(r) = f.strip_prefix(&rel_base) else { continue };
            let name = r.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let s = fuzzy(&name, q).max(fuzzy(&r.to_string_lossy(), q) / 2);
            if s > 0 {
                out.push(Completion {
                    path: r.to_string_lossy().into_owned(),
                    kind: "file",
                    size: None,
                    mtime: 0,
                    score: s - r.components().count() as i64,
                    insert: None,
                });
            }
            for a in r.ancestors().skip(1) {
                if a.as_os_str().is_empty() || !dirs.insert(a.to_path_buf()) {
                    continue;
                }
                let dn = a.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let s = fuzzy(&dn, q);
                if s > 0 {
                    out.push(Completion {
                        path: format!("{}/", a.to_string_lossy()),
                        kind: "dir",
                        size: None,
                        mtime: 0,
                        score: s + 5 - a.components().count() as i64,
                        insert: None,
                    });
                }
            }
        }
        out.sort_by(|a, b| b.score.cmp(&a.score).then(a.path.len().cmp(&b.path.len())));
        out.truncate(limit);
        for c in out.iter_mut() {
            let p = canon.join(c.path.trim_end_matches('/'));
            if let Some(m) = meta(&p) {
                c.size = m.is_file().then_some(m.len());
                c.mtime = crate::util::mtime_ms(&p);
            }
        }
        out.retain(|c| !denied(Path::new(&c.path)));
        return Ok(out);
    }
    out.sort_by(|a, b| b.score.cmp(&a.score).then(a.path.cmp(&b.path)));
    out.truncate(limit);
    Ok(out)
}

/// How a harness takes a file reference typed in its prompt. omp (source: `@path`, `@"a b"`
/// after whitespace/start; files are read into the turn, images attached, directories listed,
/// >25 MB skipped); claude `@path`; codex and agy get the plain path.
pub fn mention_format(agent_type: &str) -> serde_json::Value {
    let reads = crate::harness::get(agent_type).is_some_and(|h| h.reads_mentions());
    if reads {
        serde_json::json!({"insert": "@{path}", "quoted": "@\"{path}\"", "reads": true, "dirs": "listing"})
    } else {
        serde_json::json!({"insert": "{path}", "quoted": "\"{path}\"", "reads": false, "dirs": "path"})
    }
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            if out.len() >= INDEX_CAP {
                return out;
            }
            let Ok(ft) = e.file_type() else { continue };
            let p = e.path();
            if ft.is_dir() {
                if !SKIP_DIRS.iter().any(|s| e.file_name() == *s) {
                    stack.push(p);
                }
            } else if ft.is_file() {
                if let Ok(rel) = p.strip_prefix(root) {
                    out.push(rel.to_path_buf());
                }
            }
        }
    }
    out
}

fn denied(q: &Path) -> bool {
    let name = q.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    q.components().any(|c| matches!(c, Component::Normal(s) if s == ".ssh" || s == ".gnupg"))
        || name.starts_with("credentials")
        || name.ends_with(".pem")
        || name.ends_with(".key")
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `file://`, `~`, `#frag`/`?query` and a trailing `:12` / `:12-30` line selector removed.
fn normalize(raw: &str) -> String {
    let raw = raw.trim().trim_matches(['`', '"', '\'']);
    let p = match raw.strip_prefix("file://") {
        Some(rest) => percent_decode(rest.strip_prefix("localhost").unwrap_or(rest)),
        None => raw.to_string(),
    };
    let mut p = p.split(['#', '?']).next().unwrap_or("").to_string();
    if let Some((head, tail)) = p.rsplit_once(':') {
        if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit() || c == '-' || c == ',' || c == '+') {
            p = head.to_string();
        }
    }
    p
}

fn expand(p: &str, base: &Path) -> PathBuf {
    let home = home();
    if p == "~" {
        home
    } else if let Some(r) = p.strip_prefix("~/") {
        home.join(r)
    } else if Path::new(p).is_absolute() {
        PathBuf::from(p)
    } else {
        base.join(p)
    }
}

pub struct Ctx<'a> {
    pub worktree: &'a str,
    pub cwd: Option<&'a Path>,
    /// path-like tool arguments, newest first
    pub tool_paths: &'a [String],
    pub index: &'a Mutex<FileIndex>,
}

impl Ctx<'_> {
    fn roots(&self) -> Vec<PathBuf> {
        [Path::new(self.worktree), home().as_path()].iter().filter_map(|r| r.canonicalize().ok()).collect()
    }

    /// Canonical, allowed, existing path — or the reason it is not.
    fn check(&self, p: &Path, roots: &[PathBuf]) -> Result<PathBuf, (u16, String)> {
        if denied(p) {
            return Err((403, "denied: credential location".into()));
        }
        let canon = p.canonicalize().map_err(|_| (404, format!("not found: {}", p.display())))?;
        if !roots.iter().any(|r| canon.starts_with(r)) && !is_user_tmp_file(&canon) {
            return Err((403, "denied: outside worktree and $HOME".into()));
        }
        if denied(&canon) {
            return Err((403, "denied: credential location".into()));
        }
        Ok(canon)
    }

    /// Candidate base directories for relative paths, in priority order.
    fn bases(&self) -> Vec<PathBuf> {
        let wt = PathBuf::from(self.worktree);
        let mut out = vec![wt.clone()];
        if let Some(c) = self.cwd {
            out.push(c.to_path_buf());
        }
        let anchor = self.cwd.map(Path::to_path_buf).unwrap_or(wt);
        for t in self.tool_paths {
            let n = normalize(t);
            if n.is_empty() || n.split('/').any(|c| c == "..") {
                continue;
            }
            let p = expand(&n, &anchor);
            let dir = if p.is_dir() { Some(p.clone()) } else { p.parent().map(Path::to_path_buf) };
            // the dir and its ancestors up to the worktree, or up to $HOME for directories
            // outside it (another repository the agent works in): `_guide/x.md` written
            // relative to a subproject is found from a tool call on `<subproject>/_guide/y.md`
            if let Some(d) = dir.filter(|d| d.is_dir()) {
                let wt = Path::new(self.worktree);
                let home = home();
                let stop: &Path = if d.starts_with(wt) { wt } else { &home };
                if d.starts_with(stop) {
                    for anc in d.ancestors() {
                        if !anc.starts_with(stop) || anc == stop {
                            break;
                        }
                        out.push(anc.to_path_buf());
                    }
                }
            }
            if out.len() >= 60 {
                break;
            }
        }
        let mut seen = HashSet::new();
        out.retain(|p| seen.insert(p.clone()));
        out
    }

    pub fn resolve(&self, raw: &str) -> Result<Resolved, (u16, String)> {
        let p = normalize(raw);
        if p.is_empty() {
            return Err((400, "path required".into()));
        }
        if Path::new(&p).components().any(|c| matches!(c, Component::ParentDir)) {
            return Err((403, "denied: '..' in path".into()));
        }
        let roots = self.roots();
        let single = |path: PathBuf| Resolved { path, ambiguous: false, candidates: vec![] };
        // (1) absolute, ~ and file://
        if p == "~" || p.starts_with("~/") || Path::new(&p).is_absolute() {
            return self.check(&expand(&p, Path::new(self.worktree)), &roots).map(single);
        }
        // (2)–(4) worktree, session cwd, recent tool-call directories
        let mut first_denial = None;
        for base in self.bases() {
            match self.check(&base.join(&p), &roots) {
                Ok(c) => return Ok(single(c)),
                Err((403, e)) if first_denial.is_none() => first_denial = Some((403, e)),
                Err(_) => {}
            }
        }
        if let Some(d) = first_denial {
            return Err(d);
        }
        // (5) suffix match inside the worktree file index
        let root = Path::new(self.worktree).canonicalize().map_err(|_| (404, format!("not found: {p}")))?;
        let want: Vec<&str> = p.trim_start_matches("./").split('/').filter(|c| !c.is_empty() && *c != ".").collect();
        let files = self.index.plock().files(&root);
        let mut hits: Vec<(i64, PathBuf)> = files
            .iter()
            .filter(|f| {
                let comps: Vec<String> = f.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
                comps.len() >= want.len() && comps[comps.len() - want.len()..].iter().zip(&want).all(|(a, b)| a == b)
            })
            .filter_map(|f| self.check(&root.join(f), &roots).ok())
            .map(|c| (crate::util::mtime_ms(&c), c))
            .collect();
        if hits.is_empty() {
            return Err((404, format!("not found: {p}")));
        }
        hits.sort_by_key(|h| std::cmp::Reverse(h.0));
        let ambiguous = hits.len() > 1;
        let candidates = if ambiguous { hits.iter().take(MAX_CANDIDATES).map(|h| h.1.to_string_lossy().into_owned()).collect() } else { vec![] };
        Ok(Resolved { path: hits.swap_remove(0).1, ambiguous, candidates })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_in_another_repository_resolve_through_recent_tool_calls() {
        // HOME/work/main (the agent's worktree) and HOME/work/other (where it actually works)
        let home = std::env::temp_dir().join(format!("adash-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let main = home.join("work/main");
        let shots = home.join("work/other/_scratch/design/argon");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::create_dir_all(&shots).unwrap();
        std::fs::write(shots.join("dir1_split.png"), "a").unwrap();
        std::fs::write(shots.join("dir2_triage.png"), "b").unwrap();
        let old_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", &home);
        let idx = Mutex::new(FileIndex::default());
        let wt = main.to_string_lossy().into_owned();
        // the agent ran `cd ../other && ls _scratch/design/argon`
        let tp: Vec<String> = crate::transcript::tool_paths_of(
            "bash",
            &serde_json::json!({"command": format!("cd {} && ls _scratch/design/argon", home.join("work/other").display())}),
        );
        assert!(tp.iter().any(|p| p.ends_with("work/other/")), "{tp:?}");
        let ctx = Ctx { worktree: &wt, cwd: Some(&main), tool_paths: &tp, index: &idx };
        let r = ctx.resolve("_scratch/design/argon/dir1_split.png").unwrap();
        assert!(r.path.ends_with("work/other/_scratch/design/argon/dir1_split.png"));
        // a bare file name after that relative path in the same message: its folder is the base
        let tp2 = vec![shots.join("dir1_split.png").to_string_lossy().into_owned()];
        let ctx2 = Ctx { worktree: &wt, cwd: Some(&main), tool_paths: &tp2, index: &idx };
        assert!(ctx2.resolve("dir2_triage.png").unwrap().path.ends_with("argon/dir2_triage.png"));
        match old_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn subproject_relative_path_found_by_suffix_and_newest_wins() {
        let base = std::env::temp_dir().join(format!("adash-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let a = base.join("2027/survey_app/_guide");
        let b = base.join("2026/old_app/_guide");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::create_dir_all(base.join("target/_guide")).unwrap();
        std::fs::write(b.join("proposal_notes_v4.md"), "old").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(a.join("proposal_notes_v4.md"), "new").unwrap();
        std::fs::write(base.join("target/_guide/proposal_notes_v4.md"), "skipped dir").unwrap();
        let idx = Mutex::new(FileIndex::default());
        let wt = base.to_string_lossy().into_owned();
        let ctx = Ctx { worktree: &wt, cwd: None, tool_paths: &[], index: &idx };
        let r = ctx.resolve("`_guide/proposal_notes_v4.md`").unwrap();
        assert!(r.path.ends_with("2027/survey_app/_guide/proposal_notes_v4.md"));
        assert!(r.ambiguous);
        assert_eq!(r.candidates.len(), 2, "target/ is excluded from the index");
        // a recent tool call in the subproject makes it a direct (unambiguous) hit
        let tp = vec![format!("{}/2027/survey_app/_guide/other.md", wt)];
        let ctx = Ctx { worktree: &wt, cwd: None, tool_paths: &tp, index: &idx };
        let r = ctx.resolve("_guide/proposal_notes_v4.md:12-30").unwrap();
        assert!(r.path.ends_with("2027/survey_app/_guide/proposal_notes_v4.md") && !r.ambiguous);
        assert_eq!(ctx.resolve("../etc/passwd").unwrap_err().0, 403);
        let _ = std::fs::remove_dir_all(&base);
    }
}

/// Temp directories whose files may be served: /tmp and $TMPDIR (canonical).
fn tmp_dirs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = vec![PathBuf::from("/tmp")];
    if let Some(t) = std::env::var_os("TMPDIR") {
        v.push(PathBuf::from(t));
    }
    v.into_iter().filter_map(|d| d.canonicalize().ok()).collect()
}

/// A file agents leave in /tmp (renders, comparison images): allowed only when `canon` — already
/// symlink-resolved, so a link pointing out of /tmp fails here — is a regular file inside a temp
/// dir, owned by the user adash runs as, with a viewable type (images, PDF, Markdown, text).
pub fn is_user_tmp_file(canon: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(md) = std::fs::metadata(canon) else { return false };
    let me = crate::platform::my_uid();
    let ext = canon.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    md.is_file()
        && md.uid() == me
        && tmp_dirs().iter().any(|t| canon.starts_with(t))
        && matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "pdf" | "md" | "markdown" | "txt")
}

#[cfg(test)]
mod tmp_tests {
    use super::*;

    #[test]
    fn user_tmp_files_allowed_others_denied() {
        let dir = PathBuf::from("/tmp").join(format!("adash-tmp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("variants3.png");
        std::fs::write(&img, b"png").unwrap();
        std::fs::write(dir.join("notes.sh"), b"echo").unwrap();
        assert!(is_user_tmp_file(&img.canonicalize().unwrap()));
        // other types and directories stay out
        assert!(!is_user_tmp_file(&dir.join("notes.sh").canonicalize().unwrap()));
        assert!(!is_user_tmp_file(&dir.canonicalize().unwrap()));
        // a symlink in /tmp pointing outside resolves outside → not a tmp file
        let link = dir.join("escape.png");
        // /etc/hosts exists on Linux and macOS (/etc/hostname is Linux-only)
        std::os::unix::fs::symlink("/etc/hosts", &link).unwrap();
        assert!(!is_user_tmp_file(&link.canonicalize().unwrap()));
        // a file owned by another user (root owns /tmp itself; pick any root-owned regular file
        // under /tmp if present, else check the uid rule on a root-owned file elsewhere)
        let foreign = std::fs::read_dir("/tmp").ok().and_then(|rd| {
            use std::os::unix::fs::MetadataExt;
            rd.flatten().map(|e| e.path()).find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.uid() != crate::platform::my_uid()))
        });
        if let Some(f) = foreign {
            assert!(!is_user_tmp_file(&f.canonicalize().unwrap()), "{f:?} belongs to another user");
        }
        // through the resolver: absolute /tmp path ok; the escaping link is denied
        let idx = Mutex::new(FileIndex::default());
        let wt = dir.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        let wts = wt.to_string_lossy().into_owned();
        let ctx = Ctx { worktree: &wts, cwd: None, tool_paths: &[], index: &idx };
        assert!(ctx.resolve(&img.to_string_lossy()).is_ok());
        assert_eq!(ctx.resolve(&link.to_string_lossy()).unwrap_err().0, 403);
        assert_eq!(ctx.resolve(&dir.join("notes.sh").to_string_lossy()).unwrap_err().0, 403);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod complete_tests {
    use super::*;

    #[test]
    fn completes_by_directory_and_fuzzy_and_skips_ignored() {
        let base = std::env::temp_dir().join(format!("adash-complete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("web/vendor")).unwrap();
        std::fs::create_dir_all(base.join("node_modules/x")).unwrap();
        std::fs::write(base.join("web/app.js"), "a").unwrap();
        std::fs::write(base.join("web/style.css"), "b").unwrap();
        std::fs::write(base.join("node_modules/x/app.js"), "c").unwrap();
        std::fs::write(base.join("id.pem"), "k").unwrap();
        let idx = Mutex::new(FileIndex::default());
        let paths = |q: &str| complete(&idx, &base, &base, q, 50, false).unwrap().into_iter().map(|c| c.path).collect::<Vec<_>>();
        assert_eq!(paths("web/ap"), ["web/app.js"]);
        assert!(paths("web/").contains(&"web/vendor/".to_string()));
        let bare = paths("app");
        assert_eq!(bare.first().map(String::as_str), Some("web/app.js"), "{bare:?}");
        assert!(!bare.iter().any(|p| p.contains("node_modules")));
        assert!(!paths("id").iter().any(|p| p.ends_with(".pem")), "credential files never offered");
        assert_eq!(mention("omp", "a b.md"), "@\"a b.md\"");
        assert_eq!(mention("codex", "web/app.js"), "web/app.js");
        let _ = std::fs::remove_dir_all(&base);
    }
}
