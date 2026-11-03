// Tuple-heavy internal state and a few wide internal functions are deliberate.
#![allow(clippy::type_complexity, clippy::too_many_arguments, clippy::needless_range_loop)]
mod agy;
mod analytics;
mod ask;
mod btw;
mod collect;
mod commands;
mod config;
mod decisions;
mod demo;
mod fun;
mod harness;
mod history;
mod i18n;
mod launch;
mod lifecycle;
mod magi;
mod meta;
mod mobile;
mod network;
mod notify;
mod palette;
mod paths;
mod platform;
mod push;
mod registry;
mod report;
mod restore;
mod server;
mod session;
mod state;
mod stats;
mod term;
mod theme;
mod theme_import;
mod threads;
mod transcript;
mod uploads;
mod usage;
mod usage_hourly;
mod util;

use collect::{my_handle, Collector, Snapshot};
use meta::Meta;
use serde_json::{json, Value};
use util::{first_line, http, now_ms, server_up, truncate, Res};

const USAGE: &str = "adash — agent dashboard (tmux backend)

  adash serve [--port 7878]          HTTP UI/API + poller, notifier, relay
  adash demo [--port 7879]           the dashboard with built-in sample data (no tmux, no agents)
  adash config init|path|show|schema [--force]   write a commented config.toml / where it is / effective values / JSON Schema
  adash theme list | new <name> [--from dark] | check <file.toml> | export <name> | show <name> | import <source> [file|name] [--name N] [--light] [--dry-run] [--set-default] | schema   themes (TOML)
  adash harnesses                    supported harnesses, whether enabled, tested versions
  adash install-service              per-user services for the tmux server and `adash serve` (systemd / launchd)
  adash ls [--json] [--all]          내 차례 queue (or every agent)
  adash whoami                       caller's agent name (flags ambiguous names)
  adash iam \"<이름>\" [--role \"<역할>\"]  set your own dashboard name (2–20 chars, unique)
  adash name-request <NAME> | --ambiguous   ask agent(s) to name themselves (once per 24 h)
  adash name <NAME|handle> <NEW>     rename (persisted, mirrored to the tmux window name)
  adash focus <NAME>                 show the agent's terminal
  adash sessions [query…] [--limit 20] [--json]   past sessions by title / name / project / first prompt (no query: most recent)
  adash reopen <NAME|session-file>   resume a closed or past session in a new terminal
  adash tell <TARGET> \"text\" [--force]   untracked message (delivery gate; --force skips the input-box check and steers a working agent)
  adash ask <TARGET> \"request\" [--file brief.md] [--from NAME] [--thread ID] [--wait [--timeout 30m]] [--magi-run ID --kind review|propose --mode accept|discuss|propose]
  adash reply <THREAD> [--wait] [--timeout 30m]   print a thread's reply (marks it delivered; no terminal injection)
  adash threads [--json]
  adash thread register --from H --to NAME --marker M --brief F --kind review|propose [--mode m]   (peer_review/magi hook)
  adash thread answered --marker M [--answer FILE | < stdin]
  adash answer <NAME|handle> --body '{\"answers\":[…]}'   answer an open ask (all questions)
  adash report [--period week|month|custom] [--from YYYY-MM-DD --to …] [--prev] [--pdf]
  adash analytics backfill           ingest all session files/history into the analytics store
  adash stats [--from YYYY-MM-DD] [--to …] [--by agent|project|model|type|day|hour|weekday|pair] [--agent-to-agent] [--top N] [--json]
  adash term new --cwd DIR [--project NAME] [--title T] [--command CMD]   open a terminal → {handle}
  adash term list | whoami | close HANDLE | focus HANDLE
  adash term send HANDLE --text T [--enter]    literal input (--enter: Enter after it)
  adash term paste HANDLE --text T             bracketed paste + Enter
  adash term read HANDLE [--lines N]           rendered screen (or last N lines incl. scrollback)
  adash term wait HANDLE [--quiet-ms 1500] [--timeout-ms 120000]   until the screen is quiet (exit 2 on timeout)";

struct Args {
    pos: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    fn parse(raw: Vec<String>, valued: &[&str]) -> Args {
        let mut pos = vec![];
        let mut flags = vec![];
        let mut it = raw.into_iter();
        while let Some(a) = it.next() {
            if let Some(f) = a.strip_prefix("--") {
                if let Some((k, v)) = f.split_once('=') {
                    flags.push((k.to_string(), Some(v.to_string())));
                } else if valued.contains(&f) {
                    flags.push((f.to_string(), it.next()));
                } else {
                    flags.push((f.to_string(), None));
                }
            } else {
                pos.push(a);
            }
        }
        Args { pos, flags }
    }
    fn has(&self, k: &str) -> bool {
        self.flags.iter().any(|f| f.0 == k)
    }
    fn get(&self, k: &str) -> Option<String> {
        self.flags.iter().find(|f| f.0 == k).and_then(|f| f.1.clone())
    }
}

/// Fetch a thread's reply and mark it delivered by pull (server API when running, else on disk).
/// Ok(None) = not answered yet.
fn pull_reply(id: &str) -> Res<Option<String>> {
    if server_up() {
        let (code, v) = http("POST", &format!("/api/threads/{id}/pull"), Some(&json!({})))?;
        return match code {
            200 => Ok(v["reply"].as_str().map(String::from)),
            409 => Ok(None),
            _ => Err(v["error"].as_str().map(String::from).unwrap_or_else(|| v.to_string())),
        };
    }
    let mut t = threads::load(id).ok_or_else(|| format!("unknown thread {id}"))?;
    match threads::pull(&mut t) {
        Ok(r) => {
            t.save()?;
            Ok(Some(r))
        }
        Err(_) => Ok(None),
    }
}

/// "30m" / "90s" / "2h" / plain seconds.
fn parse_duration(s: &str) -> Res<std::time::Duration> {
    let (num, mul) = match s.trim().chars().last() {
        Some('s') => (&s[..s.len() - 1], 1),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3600),
        _ => (s, 1),
    };
    let n: u64 = num.trim().parse().map_err(|_| format!("bad --timeout {s} (e.g. 30m, 90s, 2h)"))?;
    Ok(std::time::Duration::from_secs(n * mul))
}

/// Block until the reply exists, print it on stdout (exit 0); on timeout exit 2.
fn wait_reply(id: &str, timeout: Option<&str>) -> Res<()> {
    let limit = parse_duration(timeout.unwrap_or("30m"))?;
    let start = std::time::Instant::now();
    loop {
        if let Some(r) = pull_reply(id)? {
            print!("{r}");
            return Ok(());
        }
        if start.elapsed() >= limit {
            eprintln!("adash: no reply for {id} within {}s; fetch it later with `adash reply {id}` (or `adash reply {id} --wait`)", limit.as_secs());
            std::process::exit(2);
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
}

fn collect_now() -> Res<(Collector, Snapshot)> {
    let mut c = Collector::new();
    c.persist = !server_up();
    let s = c.collect(&Meta::load())?;
    Ok((c, s))
}

fn whoami_name(snap: &Snapshot) -> Option<String> {
    let h = my_handle()?;
    snap.agents.iter().find(|a| a.handle.as_deref() == Some(h.as_str())).map(|a| a.name.clone())
}

fn api_or<F: FnOnce() -> Res<Value>>(method: &str, path: &str, body: Value, direct: F) -> Res<Value> {
    if server_up() {
        let (code, v) = http(method, path, Some(&body))?;
        if code >= 400 {
            return Err(v["error"].as_str().map(String::from).unwrap_or_else(|| v.to_string()));
        }
        Ok(v)
    } else {
        direct()
    }
}

fn fmt_since(ms: i64) -> String {
    let m = (now_ms() - ms).max(0) / 60_000;
    if m < 60 {
        format!("{m}m")
    } else if m < 48 * 60 {
        format!("{}h{:02}m", m / 60, m % 60)
    } else {
        format!("{}d", m / 1440)
    }
}

fn pad(s: &str, w: usize) -> String {
    let t = truncate(s, w.saturating_sub(2).max(1));
    let n = t.chars().count();
    format!("{t}{}", " ".repeat(w.saturating_sub(n)))
}

fn cmd_ls(a: &Args) -> Res<()> {
    let (_, snap) = collect_now()?;
    let meta = Meta::load();
    let threads = threads::load_all();
    let st = state::build(&snap, &meta, &threads);
    if a.has("json") {
        let v = if a.has("all") {
            st
        } else {
            json!({"generatedAt": st["generatedAt"], "priorityFormula": st["priorityFormula"], "queue": st["queue"], "agents": snap.agents.iter().filter(|x| x.needs_human).collect::<Vec<_>>()})
        };
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        return Ok(());
    }
    println!(
        "{}{}{}{}{}{}LAST MESSAGE / TITLE",
        pad("NAME (? = ambiguous)", 28),
        pad("PROJECT", 18),
        pad("TYPE", 12),
        pad("STATE", 9),
        pad("WAITED", 8),
        pad("CACHE", 8)
    );
    let row = |x: &collect::Agent| {
        let last = if x.last_message.trim().is_empty() { x.title.clone() } else { first_line(&x.last_message, 70) };
        let prog = x.progress.as_ref().map(|p| format!(" [{}/{}]", p.done, p.total)).unwrap_or_default();
        println!(
            "{}{}{}{}{}{}{}{}",
            pad(&format!("{}{}", x.name, if x.ambiguous { " ?" } else { "" }), 28),
            pad(&x.project, 18),
            pad(&x.agent_type, 12),
            pad(&x.state, 9),
            pad(&if x.state == "working" || x.state == "idle" { "-".into() } else { fmt_since(x.state_since) }, 8),
            pad(x.cache.as_ref().and_then(|c| c["state"].as_str()).unwrap_or("-"), 8),
            truncate(&last, 70),
            prog
        );
    };
    if a.has("all") {
        snap.agents.iter().for_each(row);
    } else {
        for q in st["queue"].as_array().unwrap() {
            match q["kind"].as_str() {
                Some("agent") => {
                    if let Some(x) = snap.agents.iter().find(|x| x.name == q["ref"].as_str().unwrap_or("")) {
                        row(x);
                    }
                }
                Some(k) => println!(
                    "{}{}{}{}{}{}{}",
                    pad(q["ref"].as_str().unwrap_or(""), 16),
                    pad(q["project"].as_str().unwrap_or(""), 18),
                    pad(k, 12),
                    pad("-", 9),
                    pad("-", 8),
                    pad("-", 8),
                    q["title"].as_str().unwrap_or("")
                ),
                None => {}
            }
        }
    }
    Ok(())
}

fn need(a: &Args, n: usize) -> Res<()> {
    if a.pos.len() < n {
        Err(format!("missing arguments\n\n{USAGE}"))
    } else {
        Ok(())
    }
}

/// Per-user services: the tmux server (its own service, so restarting adash never kills the
/// agents) and `adash serve`. systemd --user on Linux, launchd agents on macOS.
fn install_service() -> Res<()> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let h = util::home();
    let path = format!("{h}/.local/bin:{h}/.cargo/bin:{h}/.bun/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin", h = h.display());
    let conf = platform::config_dir().join("tmux.conf");
    util::atomic_write(&conf, term::TMUX_CONF.as_bytes())?;
    println!("wrote {}", conf.display());
    let tmux = term::tmux_bin().to_string_lossy().into_owned();
    let p = platform::install_service(
        term::UNIT,
        "adash tmux server",
        &[tmux, "-D".into(), "-L".into(), term::SOCKET.into(), "-f".into(), conf.to_string_lossy().into_owned()],
        &path,
        None,
    )?;
    println!("wrote {}", p.display());
    // start, never restart, the tmux service: a restart kills every agent terminal
    platform::start_service(term::UNIT, false)?;
    let p = platform::install_service(
        "adash",
        "adash — agent dashboard",
        &[exe.to_string_lossy().into_owned(), "serve".into()],
        &path,
        Some(&format!("{}.service", term::UNIT)),
    )?;
    println!("wrote {}", p.display());
    platform::start_service("adash", true)?;
    println!("{} and adash services installed and started → http://127.0.0.1:{}", term::UNIT, util::port());
    Ok(())
}

/// `adash theme …`: list, write a template, check a file, print a theme's CSS.
fn cmd_theme(a: &Args) -> Res<()> {
    let show = |t: &theme::Theme| {
        for e in &t.errors {
            println!("  error: {e}");
        }
        for w in &t.warnings {
            println!("  warning: {w}");
        }
    };
    match a.pos.first().map(String::as_str) {
        Some("list") | None => {
            let (themes, refused) = theme::load_all_with_refused();
            for (p, r) in refused {
                println!("{:<16} refused  {}\n  error: {r}", "-", p.display());
            }
            for t in themes {
                println!("{:<16} {:<8} {}", t.name, t.source, t.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default());
                show(&t);
            }
            Ok(())
        }
        Some("new") => {
            let name = a.pos.get(1).ok_or("usage: adash theme new <name> [--from dark]")?;
            if !theme::valid_name(name) {
                return Err("theme names use letters, digits, - and _".into());
            }
            let from = a.get("from").unwrap_or_else(|| "dark".into());
            let p = theme::user_dir().join(format!("{name}.toml"));
            if p.exists() && !a.has("force") {
                return Err(format!("{} exists (use --force to overwrite)", p.display()));
            }
            util::atomic_write(&p, theme::template(name, &from).as_bytes())?;
            println!("wrote {}", p.display());
            Ok(())
        }
        Some("check") => {
            let f = a.pos.get(1).ok_or("usage: adash theme check <file.toml>")?;
            let t = theme::check_file(std::path::Path::new(f))?;
            println!("{} (base {})", t.name, t.base.as_deref().unwrap_or("light"));
            show(&t);
            if t.errors.is_empty() {
                println!("ok");
                Ok(())
            } else {
                Err(format!("{} error(s): the dashboard shows the validated base theme (or light) instead", t.errors.len()))
            }
        }
        Some("export") => {
            let n = a.pos.get(1).ok_or("usage: adash theme export <name>")?;
            let t = theme::load_all().into_iter().find(|t| &t.name == n).ok_or_else(|| format!("unknown theme {n}"))?;
            print!("{}", theme::css(&t));
            Ok(())
        }
        Some("show") => {
            let n = a.pos.get(1).ok_or("usage: adash theme show <name>")?;
            let text = theme::builtins()
                .iter()
                .find(|b| &b.0 == n)
                .map(|b| b.1.clone())
                .or_else(|| theme::load_all().into_iter().find(|t| &t.name == n).and_then(|t| t.path).and_then(|p| std::fs::read_to_string(p).ok()));
            print!("{}", text.ok_or_else(|| format!("unknown theme {n}"))?);
            Ok(())
        }
        Some("import") => {
            let src = a.pos.get(1).ok_or_else(|| {
                format!(
                    "usage: adash theme import <{}> [file|name] [--name N] [--light] [--dry-run] [--set-default] [--force]",
                    theme_import::SOURCES.join("|")
                )
            })?;
            let want_dark = !a.has("light");
            let i = theme_import::import(src, a.pos.get(2).map(String::as_str), a.get("name").as_deref(), want_dark)?;
            for r in &i.report {
                println!("  {r}");
            }
            // the output must parse; a theme with errors is neither written nor made the default
            i.text.parse::<toml::Table>().map_err(|e| format!("generated theme does not parse ({e}); nothing written"))?;
            let (w, e) = theme::check_text(&i.palette.id, &i.text);
            for x in &e {
                println!("  error: {x}");
            }
            for x in &w {
                println!("  warning: {x}");
            }
            if a.has("dry-run") {
                print!("{}", i.text);
                return if e.is_empty() { Ok(()) } else { Err(format!("{} error(s); a real import would write nothing", e.len())) };
            }
            if !e.is_empty() {
                return Err(format!("{} error(s); nothing written", e.len()));
            }
            let p = theme::user_dir().join(format!("{}.toml", i.palette.id));
            if p.exists() && !a.has("force") {
                return Err(format!("{} exists (use --force to overwrite, or --name)", p.display()));
            }
            util::atomic_write(&p, i.text.as_bytes())?;
            println!("wrote {} (theme \"{}\")", p.display(), i.palette.id);
            if a.has("set-default") {
                let c = config::set_theme_default(i.palette.dark, &i.palette.id)?;
                println!("set [theme] {} = \"{}\" in {}", if i.palette.dark { "dark" } else { "light" }, i.palette.id, c.display());
            }
            Ok(())
        }
        Some("schema") => {
            println!("{}", serde_json::to_string_pretty(&theme::schema()).unwrap());
            Ok(())
        }
        Some(o) => Err(format!("unknown theme command {o} (list|new|check|export|schema)")),
    }
}

/// `adash term …`: direct tmux operations (no server needed); JSON on stdout.
fn cmd_term(a: &Args) -> Res<()> {
    let sub = a.pos.first().map(String::as_str).unwrap_or("");
    let handle = || a.pos.get(1).cloned().ok_or_else(|| format!("usage: adash term {sub} HANDLE"));
    let text = || a.get("text").ok_or_else(|| "--text required".to_string());
    let out = |v: Value| println!("{v}");
    match sub {
        "new" => {
            let dir = a.get("cwd").ok_or("--cwd required")?;
            let cwd = std::fs::canonicalize(&dir).map_err(|e| format!("{dir}: {e}"))?;
            if !cwd.is_dir() {
                return Err(format!("{}: not a directory", cwd.display()));
            }
            let cwd_s = cwd.to_string_lossy().into_owned();
            let project = a
                .get("project")
                .or_else(|| Meta::load().project_for_path(&cwd_s))
                .or_else(|| cwd.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "adash".into());
            let h = term::create(&cwd, &project, &a.get("title").unwrap_or_default(), a.get("command").as_deref(), None)?;
            out(json!({"handle": h}));
        }
        "list" => out(Value::Array(term::panes()?.iter().map(term::Pane::to_json).collect())),
        "send" => {
            let (h, t) = (handle()?, text()?);
            let mut v = term::send_text(&h, &t)?;
            if a.has("enter") {
                v = term::send_enter(&h)?;
            }
            out(v);
        }
        "paste" => out(term::send_paste(&handle()?, &text()?)?),
        "read" => {
            let h = handle()?;
            let lines = match a.get("lines") {
                Some(n) => term::tail_n(&h, n.parse().map_err(|_| format!("--lines {n}: not a number"))?)?.1,
                None => term::screen(&h)?,
            };
            out(json!({"source": "screen", "lines": lines}));
        }
        "wait" => {
            let h = handle()?;
            let num = |k: &str, d: i64| a.get(k).map(|v| v.parse::<i64>().map_err(|_| format!("--{k} {v}: not a number"))).transpose().map(|v| v.unwrap_or(d));
            let (quiet, timeout) = (num("quiet-ms", 1500)?, num("timeout-ms", 120_000)?);
            let t0 = now_ms();
            let mut last = term::screen(&h)?;
            let mut since = t0;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(250));
                let now = now_ms();
                let cur = term::screen(&h)?;
                if cur != last {
                    last = cur;
                    since = now;
                } else if now - since >= quiet {
                    out(json!({"idle": true, "waitedMs": now - t0}));
                    return Ok(());
                }
                if now - t0 >= timeout {
                    out(json!({"idle": false, "waitedMs": now - t0}));
                    std::process::exit(2);
                }
            }
        }
        "close" => {
            let h = handle()?;
            term::close(&h)?;
            out(json!({"ok": true, "handle": h}));
        }
        "focus" => out(term::focus(&handle()?)?),
        "whoami" => out(json!({"handle": term::env_handle()})),
        _ => return Err("usage: adash term new|list|send|paste|read|wait|close|focus|whoami (see adash help)".into()),
    }
    Ok(())
}

fn run() -> Res<()> {
    let mut raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() {
        println!("{USAGE}");
        return Ok(());
    }
    let cmd = raw.remove(0);
    let a = Args::parse(
        raw,
        &[
            "port",
            "file",
            "from",
            "thread",
            "role",
            "to",
            "by",
            "top",
            "timeout",
            "magi-run",
            "kind",
            "mode",
            "to-name",
            "marker",
            "brief",
            "answer",
            "body",
            "period",
            "cwd",
            "project",
            "title",
            "command",
            "text",
            "lines",
            "quiet-ms",
            "timeout-ms",
            "limit",
            "name",
        ],
    );
    match cmd.as_str() {
        "serve" => server::serve(a.get("port").and_then(|p| p.parse().ok()).unwrap_or_else(util::port)),
        "ls" => cmd_ls(&a),
        "whoami" => {
            let (_, snap) = collect_now()?;
            let h = my_handle();
            match snap.agents.iter().find(|x| x.handle.is_some() && x.handle == h) {
                Some(x) if x.ambiguous => {
                    println!("{} (ambiguous — adash iam \"<이름>\" --role \"<역할>\" 로 이름을 정하세요)", x.name);
                    Ok(())
                }
                Some(x) => {
                    println!("{}", x.name);
                    Ok(())
                }
                None => Err(format!("not running inside an adash terminal (handle: {})", h.unwrap_or_else(|| "none".into()))),
            }
        }
        "iam" => {
            need(&a, 1)?;
            let name = a.pos.join(" ");
            let role = a.get("role");
            let me = my_handle().ok_or("adash iam must run inside an adash terminal ($ADASH_HANDLE unset)")?;
            let v = api_or("POST", &format!("/api/agents/{me}/iam"), json!({"name": name, "role": role}), || {
                let (mut c, s) = collect_now()?;
                server::op_iam(&mut c, &s, &me, &name, role.as_deref())
            })?;
            println!("{} → {} (self){}", v["old"].as_str().unwrap_or(""), v["name"].as_str().unwrap_or(""), v["role"].as_str().map(|r| format!(" — {r}")).unwrap_or_default());
            Ok(())
        }
        "name-request" => {
            let all = a.has("ambiguous");
            if !all {
                need(&a, 1)?;
            }
            let k = a.pos.join(" ");
            let (path, body) = if all { ("/api/request-names".to_string(), json!({})) } else { (format!("/api/agents/{k}/request-name"), json!({})) };
            let v = api_or("POST", &path, body, || {
                let (mut c, s) = collect_now()?;
                let keys = if all { server::ambiguous_names(&s) } else { vec![k.clone()] };
                server::op_request_names(&mut c, &s, &keys)
            })?;
            println!("queued: {}", v["queued"].as_array().map(|q| q.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default());
            for s in v["skipped"].as_array().cloned().unwrap_or_default() {
                println!("skipped: {} ({})", s["name"].as_str().unwrap_or(""), s["reason"].as_str().unwrap_or(""));
            }
            if !server_up() {
                eprintln!("adash: server not running; requests are queued and sent when `adash serve` runs");
            }
            Ok(())
        }
        "name" => {
            need(&a, 2)?;
            let (k, n) = (a.pos[0].clone(), a.pos[1].clone());
            let v = api_or("POST", &format!("/api/agents/{k}/rename"), json!({"name": n}), || {
                let (mut c, s) = collect_now()?;
                server::op_rename(&mut c, &s, &k, &n)
            })?;
            println!("{} → {}{}", v["old"].as_str().unwrap_or(&k), v["name"].as_str().unwrap_or(&n), if v["mirroredToTerminal"] == true { " (tmux window renamed)" } else { "" });
            Ok(())
        }
        "focus" => {
            need(&a, 1)?;
            let k = a.pos[0].clone();
            let v = api_or("POST", &format!("/api/agents/{k}/focus"), json!({}), || server::op_focus(&collect_now()?.1, &k))?;
            println!("focused {} ({}){}", v["name"].as_str().unwrap_or(""), v["handle"].as_str().unwrap_or(""), if v["opened"] == true { " in a new terminal window" } else { "" });
            Ok(())
        }
        "sessions" => {
            let db = analytics::open_read()?;
            let hits = stats::search_sessions(&db, &a.pos.join(" "), a.get("limit").and_then(|l| l.parse().ok()).unwrap_or(20))?;
            if a.has("json") {
                println!("{}", serde_json::to_string_pretty(&hits).unwrap());
                return Ok(());
            }
            println!("{}{}{}{}SESSION FILE", pad("LAST", 8), pad("PROJECT", 18), pad("TYPE", 8), pad("TITLE", 60));
            for h in &hits {
                let title = match h["agent"].as_str() {
                    Some(n) => format!("{n} · {}", h["title"].as_str().unwrap_or("")),
                    None => h["title"].as_str().unwrap_or("").to_string(),
                };
                println!("{}{}{}{}{}", pad(&fmt_since(h["lastTs"].as_i64().unwrap_or(0)), 8), pad(h["project"].as_str().unwrap_or(""), 18), pad(h["agentType"].as_str().unwrap_or(""), 8), pad(&title, 60), h["file"].as_str().unwrap_or(""));
            }
            Ok(())
        }
        "reopen" => {
            need(&a, 1)?;
            let k = a.pos[0].clone();
            // the key travels in the body: session files contain '/'
            let v = api_or("POST", "/api/agents/_/reopen", json!({"identity": k}), || lifecycle::reopen(&collect_now()?.1, &k).map_err(|(c, v)| format!("{c}: {v}")))?;
            println!("reopened {} ({}): {}", v["name"].as_str().unwrap_or(""), v["handle"].as_str().unwrap_or(""), v["command"].as_str().unwrap_or(""));
            Ok(())
        }
        "tell" => {
            need(&a, 2)?;
            let (k, text) = (a.pos[0].clone(), a.pos[1..].join(" "));
            let force = a.has("force");
            // an agent's `tell` is not a human interaction; a plain shell is
            let from = collect_now().ok().and_then(|(_, s)| whoami_name(&s));
            let v = api_or("POST", &format!("/api/agents/{k}/send"), json!({"text": text, "force": force, "from": from, "channel": "cli-tell"}), || {
                let (_, s) = collect_now()?;
                let v = server::op_send(&s, &k, &text, &[], force)?;
                if v["delivered"] == true && from.is_none() {
                    if let Some(x) = s.agents.iter().find(|x| x.name == v["name"].as_str().unwrap_or("")) {
                        history::append_send(now_ms(), &x.identity, &text, "cli-tell");
                    }
                }
                Ok(v)
            })?;
            if v["delivered"] == true {
                println!("sent to {} (mode {}{})", v["name"].as_str().unwrap_or(&k), v["mode"].as_str().unwrap_or("line"), v["file"].as_str().map(|f| format!(", file {f}")).unwrap_or_default());
                Ok(())
            } else {
                Err(format!("held ({}{}): not sent to {}; retry with --force", v["held"].as_str().unwrap_or("?"), v["detail"].as_str().map(|d| format!(": {d}")).unwrap_or_default(), v["name"].as_str().unwrap_or(&k)))
            }
        }
        "ask" => {
            need(&a, 1)?;
            let to = a.pos[0].clone();
            let mut request = a.pos[1..].join(" ");
            if let Some(f) = a.get("file") {
                let body = std::fs::read_to_string(&f).map_err(|e| format!("{f}: {e}"))?;
                request = if request.is_empty() { body } else { format!("{request}\n\n{body}") };
            }
            let parent = a.get("thread");
            let from = match a.get("from") {
                Some(f) => Some(f),
                None => {
                    let (_, s) = collect_now()?;
                    whoami_name(&s)
                }
            };
            let opts = threads::ThreadOpts { magi_run: a.get("magi-run"), kind: a.get("kind"), mode: a.get("mode") };
            let body = json!({"from": from, "to": to, "request": request, "thread": parent, "magiRun": opts.magi_run, "kind": opts.kind, "mode": opts.mode});
            let v = api_or("POST", "/api/ask", body, || {
                let (_, s) = collect_now()?;
                Ok(server::op_ask(&s, from.as_deref(), &to, &request, parent.as_deref(), &opts)?.summary())
            })?;
            let id = v["id"].as_str().unwrap_or("").to_string();
            if a.has("wait") {
                // status line to stderr so stdout is exactly the reply
                eprintln!("{id} {} → {} [{}] waiting for the reply…", v["from"].as_str().unwrap_or(""), v["to"].as_str().unwrap_or(""), v["status"].as_str().unwrap_or(""));
                return wait_reply(&id, a.get("timeout").as_deref());
            }
            println!("{} {} → {} [{}] {}", id, v["from"].as_str().unwrap_or(""), v["to"].as_str().unwrap_or(""), v["status"].as_str().unwrap_or(""), v["dir"].as_str().unwrap_or(""));
            if !server_up() {
                eprintln!("adash: server not running; thread queued on disk and will be delivered when `adash serve` starts");
            }
            Ok(())
        }
        "report" => {
            // adash report --period week|month|custom [--from YYYY-MM-DD --to YYYY-MM-DD] [--prev] [--pdf]
            let period = a.get("period").unwrap_or_else(|| "week".into());
            let (p, label, from, to) = report::bounds(&period, a.get("from").as_deref(), a.get("to").as_deref(), a.has("prev"))?;
            let db = analytics::open()?;
            let r = report::build(&db, &p, &label, from, to, a.has("pdf"))?;
            println!("{}\n{}{}", r.id, r.html.display(), r.pdf.map(|p| format!("\n{}", p.display())).unwrap_or_default());
            Ok(())
        }
        "answer" => {
            // adash answer <agent> --body '{"answers":[{"id":"q1","optionIndex":0}]}'
            let name = a.pos.first().ok_or("usage: adash answer <agent> --body '{\"answers\":[…]}'")?.clone();
            let body: Value = serde_json::from_str(&a.get("body").ok_or("--body required")?).map_err(|e| format!("--body: {e}"))?;
            let mut v = api_or("POST", &format!("/api/agents/{}/answer", name.replace('%', "%25").replace(' ', "%20").replace('/', "%2F")), body.clone(), || {
                let (_, s) = collect_now()?;
                let ag = s.agents.iter().find(|x| x.name == name || x.handle.as_deref() == Some(name.as_str())).ok_or_else(|| format!("unknown agent {name}"))?;
                let t0 = std::time::Instant::now();
                server::op_answer(ag, &body, &|stage, d| eprintln!("adash: {:>5} ms {stage} {d}", t0.elapsed().as_millis())).map_err(|(_, e)| e)
            })?;
            // server: 202 {jobId} → poll until done
            if let Some(job) = v["jobId"].as_str().map(String::from) {
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    let (_, j) = http("GET", &format!("/api/answers/{job}"), None)?;
                    if j["state"] != "running" && j["state"] != "queued" {
                        if j["state"] == "failed" {
                            return Err(j["error"].as_str().unwrap_or("failed").to_string());
                        }
                        v = j["result"].clone();
                        break;
                    }
                }
            }
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
            Ok(())
        }
        "thread" => match a.pos.first().map(String::as_str) {
            Some("register") => {
                let body = json!({"from": a.get("from"), "to": a.get("to-name").or(a.get("to")), "marker": a.get("marker"), "brief": a.get("brief"), "kind": a.get("kind"), "mode": a.get("mode"), "role": a.get("role")});
                let v = api_or("POST", "/api/threads/register", body.clone(), || {
                    let (_, s) = collect_now()?;
                    let name_of = |k: &str| s.agents.iter().find(|x| x.name == k || x.handle.as_deref() == Some(k)).map(|x| x.name.clone()).unwrap_or_else(|| k.to_string());
                    let marker = body["marker"].as_str().ok_or("--marker required")?;
                    Ok(threads::register_external(&name_of(body["from"].as_str().unwrap_or("unknown")), &name_of(body["to"].as_str().unwrap_or("")), marker, body["brief"].as_str().unwrap_or(""), body["kind"].as_str().unwrap_or("review"), body["mode"].as_str(), now_ms(), "hook")?.summary())
                })?;
                println!("{}", v["id"].as_str().unwrap_or(""));
                Ok(())
            }
            Some("answered") => {
                let marker = a.get("marker").ok_or("--marker required")?;
                let mut text = String::new();
                if let Some(f) = a.get("answer") {
                    text = std::fs::read_to_string(&f).map_err(|e| format!("{f}: {e}"))?;
                } else if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut text).ok();
                }
                let v = api_or("POST", "/api/threads/answered", json!({"marker": marker, "text": text}), || Ok(threads::answered_external(&marker, &text)?.summary()))?;
                println!("{} {}", v["id"].as_str().unwrap_or(""), v["status"].as_str().unwrap_or(""));
                Ok(())
            }
            _ => Err("usage: adash thread register --from H --to NAME --marker M --brief F --kind review|propose [--mode m] | adash thread answered --marker M [--answer FILE | < stdin]".into()),
        },
        "reply" => {
            need(&a, 1)?;
            let id = a.pos[0].clone();
            if a.has("wait") {
                return wait_reply(&id, a.get("timeout").as_deref());
            }
            let r = pull_reply(&id)?.ok_or_else(|| format!("{id}: no reply yet (use --wait)"))?;
            print!("{r}");
            Ok(())
        }
        "threads" => {
            let ts = threads::load_all();
            if a.has("json") {
                println!("{}", serde_json::to_string_pretty(&ts.iter().map(threads::Thread::summary).collect::<Vec<_>>()).unwrap());
            } else {
                for t in &ts {
                    println!("{}  {:>6} → {:<10} {:<20} hops {}  {}", t.id, t.from, t.to, t.status, t.hops, truncate(first_line(&t.request_text(), 60).as_str(), 60));
                }
                if ts.is_empty() {
                    println!("(no threads)");
                }
            }
            Ok(())
        }
        "install-service" => install_service(),
        "theme" => cmd_theme(&a),
        "harnesses" => {
            println!("{:<12} {:<8} {:<14} binary", "HARNESS", "ENABLED", "TESTED");
            for h in harness::all() {
                let bin = match h.kind() {
                    "antigravity" => "agy",
                    k => k,
                };
                let found = std::env::var("PATH").unwrap_or_default().split(':').map(|d| std::path::Path::new(d).join(bin)).find(|p| p.is_file());
                println!("{:<12} {:<8} {:<14} {}", h.kind(), config::get().harness_enabled(h.kind()), h.tested_versions(), found.map(|p| p.display().to_string()).unwrap_or_else(|| "(not on PATH)".into()));
            }
            Ok(())
        }
        "config" => match a.pos.first().map(String::as_str) {
            Some("init") => {
                let p = config::path();
                if p.exists() && !a.has("force") {
                    return Err(format!("{} exists (use --force to overwrite)", p.display()));
                }
                // an existing config.yaml is converted, never replaced by defaults; a broken one stops here
                if config::migrate_yaml_in(&platform::config_dir())?.is_some() {
                    println!("wrote {} from config.yaml", p.display());
                    return Ok(());
                }
                util::atomic_write(&p, config::default_text().as_bytes())?;
                println!("wrote {}", p.display());
                Ok(())
            }
            Some("path") => {
                println!("{}", config::path().display());
                Ok(())
            }
            Some("schema") => {
                println!("{}", serde_json::to_string_pretty(&config::schema()).unwrap());
                Ok(())
            }
            Some("show") | None => {
                println!("{}", toml::to_string(config::get()).unwrap_or_default());
                Ok(())
            }
            Some(o) => Err(format!("unknown config command {o} (init|path|show|schema)")),
        },
        "demo" => demo::serve(a.get("port").and_then(|p| p.parse().ok()).unwrap_or(7879)),
        "term" => cmd_term(&a),
        "analytics" => match a.pos.first().map(String::as_str) {
            Some("backfill") => {
                let db = analytics::open()?;
                let ctx = analytics::context(None);
                let mut r = analytics::ingest_all(&db, &ctx)?;
                // backfill = every message re-checked for an agent sender, not just the last day
                let before: i64 = db.conn.query_row("SELECT COUNT(*) FROM human_messages WHERE kind = 'human'", [], |x| x.get(0)).unwrap_or(0);
                let markers: std::collections::HashMap<String, String> = threads::load_all().into_iter().filter_map(|t| t.marker.clone().map(|m| (m, t.from.clone()))).collect();
                r["agentMessagesLinked"] = json!(db.link_agent_messages(0, &markers)?);
                let after: i64 = db.conn.query_row("SELECT COUNT(*) FROM human_messages WHERE kind = 'human'", [], |x| x.get(0)).unwrap_or(0);
                r["humanToAgent"] = json!(before - after);
                let t = db.totals();
                println!("{}", serde_json::to_string_pretty(&json!({"run": r, "totals": t})).unwrap());
                Ok(())
            }
            _ => Err("usage: adash analytics backfill".into()),
        },
        "stats" => {
            let by = a.get("by").unwrap_or_else(|| "all".into());
            let (from, to) = stats::bounds(a.get("from").as_deref(), a.get("to").as_deref())?;
            let db = analytics::open()?;
            if a.has("agent-to-agent") || by == "pair" {
                let v = stats::pairs(&db, from, to)?;
                if a.has("json") {
                    println!("{}", serde_json::to_string_pretty(&v).unwrap());
                } else {
                    print!("{}", stats::render_pairs(&v, a.get("top").and_then(|t| t.parse().ok()).unwrap_or(40)));
                }
                return Ok(());
            }
            if by == "errors" {
                let v = stats::errors(&db, from, to)?;
                if a.has("json") {
                    println!("{}", serde_json::to_string_pretty(&v).unwrap());
                } else {
                    print!("{}", stats::render_errors(&v));
                }
                return Ok(());
            }
            let v = stats::stats(&db, from, to, &by)?;
            if a.has("json") {
                println!("{}", serde_json::to_string_pretty(&v).unwrap());
            } else {
                print!("{}", stats::render(&v, a.get("top").and_then(|t| t.parse().ok()).unwrap_or(40)));
            }
            Ok(())
        }
        "help" | "-h" | "--help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command {other}\n\n{USAGE}")),
    }
}

fn main() {
    if matches!(std::env::args().nth(1).as_deref(), Some("--version" | "-V" | "version")) {
        println!("adash {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if let Err(e) = run() {
        eprintln!("adash: {e}");
        std::process::exit(1);
    }
}
