//! Terminal backend: a private tmux server (`tmux -L adash`) run by the `adash-tmux` user unit.
//! One tmux session per project, one window per terminal. Every terminal carries a stable adash
//! handle (`t_<hex>`): the pane option `@adash_handle` and the `ADASH_HANDLE` environment
//! variable of everything started in it, so handles survive adash restarts and are reused when
//! adash restores panes after the tmux server restarted (reboot).
use crate::util::{now_ms, sanitize, LockExt, Res};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

/// tmux socket name (`tmux -L adash`): separate from the user's own tmux server.
pub const SOCKET: &str = "adash";
/// Environment variable naming the terminal a process runs in.
pub const HANDLE_ENV: &str = "ADASH_HANDLE";
/// systemd user unit that owns the tmux server (outside adash.service's cgroup, so restarting
/// adash never kills agents).
pub const UNIT: &str = "adash-tmux";

/// Server options; written to `~/.config/adash/tmux.conf` by `adash install`.
pub const TMUX_CONF: &str = "# written by `adash install`; the adash tmux server (tmux -L adash)
set -g exit-empty off
set -g default-size 220x50
set -g window-size latest
set -g history-limit 50000
set -g escape-time 0
set -g focus-events on
set -g extended-keys on
set -g allow-passthrough on
set -g set-titles on
set -g mouse on
set -g default-terminal tmux-256color
set -as terminal-features ',*:RGB'
set -g status-left '[#S] '
set -g status-left-length 40
";

/// Deadline for one tmux call: a wedged server must not freeze the poll loop or hold locks.
const CALL_TIMEOUT_S: u64 = 10;

/// The tmux binary (config `terminal.tmux`, else PATH).
pub fn tmux_bin() -> PathBuf {
    TMUX.clone()
}

static TMUX: LazyLock<PathBuf> = LazyLock::new(|| {
    let conf = &crate::config::get().terminal.tmux;
    if conf.contains('/') {
        return PathBuf::from(conf);
    }
    for dir in std::env::var("PATH").unwrap_or_default().split(':').chain(["/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"]) {
        let p = PathBuf::from(dir).join("tmux");
        if p.exists() {
            return p;
        }
    }
    PathBuf::from("/usr/bin/tmux")
});

/// Run one tmux command on the adash server; stdout on success.
pub fn tmux(args: &[&str]) -> Res<String> {
    let out = crate::platform::timeout(CALL_TIMEOUT_S)
        .arg(&*TMUX)
        .args(["-L", SOCKET])
        .args(args)
        .output()
        .map_err(|e| format!("tmux {}: {e}", args.first().unwrap_or(&"")))?;
    if crate::platform::timed_out(&out.status) {
        return Err(format!("tmux {}: timed out after {CALL_TIMEOUT_S} s", args.first().unwrap_or(&"")));
    }
    if !out.status.success() {
        return Err(format!("tmux {}: {}", args.first().unwrap_or(&""), sanitize(&out.stderr).trim()));
    }
    Ok(sanitize(&out.stdout))
}

/// Is the tmux server up?
pub fn status() -> bool {
    server_pid().is_some()
}

/// Start the server unit when it is down and wait (≤5 s) for its socket.
pub fn ensure_server() -> Res<()> {
    if server_pid().is_some() {
        harness_env();
        return Ok(());
    }
    crate::platform::start_service(UNIT, false)?;
    for _ in 0..50 {
        if server_pid().is_some() {
            harness_env();
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(format!("{UNIT} started but the tmux server did not answer"))
}

/// Environment variable that turns off omp's own desktop notifications (adash notifies instead).
pub const OMP_SILENCE: &str = "PI_NO_DESKTOP_NOTIFY";

/// `notifications.silenceHarness`: the agent server's global environment carries the omp
/// switch, so any shell opened in it (also by hand) starts omp without its own notifications.
pub fn harness_env() {
    let _ = if crate::config::get().notifications.silence_harness {
        tmux(&["set-environment", "-g", OMP_SILENCE, "1"])
    } else {
        tmux(&["set-environment", "-gu", OMP_SILENCE])
    };
}

/// An agent command with the harness's own notifications off, when the harness has a switch
/// for it: Claude Code `preferredNotifChannel` (as flag settings, so `~/.claude` is untouched),
/// Codex `tui.notifications`. omp reads `PI_NO_DESKTOP_NOTIFY` from the pane environment.
pub fn silence_harness(command: &str) -> String {
    let t = command.trim_start();
    let (prog, rest) = t.split_once(' ').unwrap_or((t, ""));
    let flag = match prog {
        "claude" if !t.contains("preferredNotifChannel") => r#"--settings '{"preferredNotifChannel":"notifications_disabled"}'"#,
        "codex" if !t.contains("tui.notifications") => "-c tui.notifications=false",
        _ => return command.to_string(),
    };
    // codex subcommands (`codex resume …`) take the global -c before them as well
    if rest.is_empty() {
        format!("{prog} {flag}")
    } else {
        format!("{prog} {flag} {rest}")
    }
}

/// The tmux server's pid (None = server down). Changes when the server restarts.
pub fn server_pid() -> Option<u32> {
    tmux(&["display-message", "-p", "#{pid}"]).ok().and_then(|s| s.trim().parse().ok())
}

/// One terminal (tmux pane).
#[derive(Clone, Debug, Default)]
pub struct Pane {
    /// adash handle (`t_<hex>`)
    pub handle: String,
    /// tmux pane id (`%12`): valid only for this server instance
    pub pane_id: String,
    /// tmux session = project
    pub session: String,
    /// window name (what the status bar shows; adash sets it to the agent name)
    pub window_name: String,
    /// title the program set (OSC 0/2), e.g. omp's "OMP > task"
    pub title: String,
    /// directory the terminal was opened in (worktree / project path)
    pub cwd: String,
    /// shell's current directory
    pub current_path: String,
    /// pid of the pane's shell
    pub pid: u32,
    /// last output (ms)
    pub activity_ms: i64,
    pub dead: bool,
}

impl Pane {
    pub fn to_json(&self) -> Value {
        json!({
            "handle": self.handle, "paneId": self.pane_id, "session": self.session, "windowName": self.window_name,
            "title": self.title, "cwd": self.cwd, "currentPath": self.current_path, "pid": self.pid,
            "lastOutputAt": self.activity_ms, "dead": self.dead,
        })
    }
}

const SEP: char = '\u{1f}';

/// Every adash terminal (panes without an adash handle are ignored).
pub fn panes() -> Res<Vec<Pane>> {
    let fmt = [
        "#{@adash_handle}",
        "#{pane_id}",
        "#{session_name}",
        "#{window_name}",
        "#{pane_title}",
        "#{@adash_cwd}",
        "#{pane_current_path}",
        "#{pane_pid}",
        "#{window_activity}",
        "#{pane_dead}",
    ]
    .join(&SEP.to_string());
    let out = match tmux(&["list-panes", "-a", "-F", &fmt]) {
        Ok(o) => o,
        // a server without sessions has no target for list-panes (the server itself is down
        // only on "no server running", which stays an error)
        Err(e) if e.contains("no current target") || e.contains("no sessions") => return Ok(vec![]),
        Err(e) => return Err(e),
    };
    Ok(out
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(SEP).collect();
            if f.len() < 10 || f[0].is_empty() {
                return None;
            }
            Some(Pane {
                handle: f[0].into(),
                pane_id: f[1].into(),
                session: f[2].into(),
                window_name: f[3].into(),
                title: f[4].into(),
                cwd: if f[5].is_empty() { f[6].into() } else { f[5].into() },
                current_path: f[6].into(),
                pid: f[7].parse().unwrap_or(0),
                activity_ms: f[8].parse::<i64>().unwrap_or(0) * 1000,
                dead: f[9] == "1",
            })
        })
        .collect())
}

/// tmux pane id of a handle.
pub fn pane_of(handle: &str) -> Res<String> {
    panes()?.into_iter().find(|p| p.handle == handle).map(|p| p.pane_id).ok_or_else(|| format!("terminal_not_found: {handle}"))
}

pub fn exists(handle: &str) -> bool {
    panes().is_ok_and(|ps| ps.iter().any(|p| p.handle == handle))
}

pub fn new_handle() -> String {
    let mut b = [0u8; 4];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut b);
    }
    format!("t_{:08x}", u32::from_le_bytes(b) ^ (now_ms() as u32))
}

/// tmux session name for a project (tmux forbids `.` and `:`).
pub fn session_name(project: &str) -> String {
    let s: String = project.chars().map(|c| if c == '.' || c == ':' || c.is_whitespace() { '_' } else { c }).collect();
    if s.is_empty() {
        "adash".into()
    } else {
        s
    }
}

/// Open a terminal in `cwd` (window in the project's session, created when missing). The shell
/// starts with `ADASH_HANDLE`; `command` is typed into it (the shell stays after it exits).
/// `handle` reuses a known handle (restore after a server restart). Returns the handle.
pub fn create(cwd: &Path, project: &str, title: &str, command: Option<&str>, handle: Option<&str>) -> Res<String> {
    ensure_server()?;
    let h = handle.filter(|h| !h.is_empty()).map(String::from).unwrap_or_else(new_handle);
    let sess = session_name(project);
    let cwd_s = cwd.to_string_lossy().into_owned();
    let env = format!("{HANDLE_ENV}={h}");
    let silence = crate::config::get().notifications.silence_harness;
    let omp_env = format!("{OMP_SILENCE}=1");
    let name = if title.trim().is_empty() { "shell" } else { title.trim() };
    let has = tmux(&["has-session", "-t", &format!("={sess}")]).is_ok();
    let tsess = format!("={sess}:");
    let pane = if has {
        let mut a = vec!["new-window", "-d", "-P", "-F", "#{pane_id}", "-t", &tsess, "-n", name, "-c", &cwd_s, "-e", &env];
        if silence {
            a.extend(["-e", omp_env.as_str()]);
        }
        tmux(&a)?
    } else {
        let mut a = vec!["new-session", "-d", "-P", "-F", "#{pane_id}", "-s", &sess, "-n", name, "-c", &cwd_s, "-e", &env];
        if silence {
            a.extend(["-e", omp_env.as_str()]);
        }
        tmux(&a)?
    };
    let pane = pane.trim().to_string();
    tmux(&["set-option", "-p", "-t", &pane, "@adash_handle", &h])?;
    tmux(&["set-option", "-p", "-t", &pane, "@adash_cwd", &cwd_s])?;
    tmux(&["set-option", "-w", "-t", &pane, "automatic-rename", "off"])?;
    if let Some(c) = command.filter(|c| !c.trim().is_empty()) {
        let c = if silence { silence_harness(c) } else { c.to_string() };
        type_line(&pane, &c)?;
    }
    Ok(h)
}

/// Close a terminal (kills what runs in it).
pub fn close(handle: &str) -> Res<()> {
    let p = pane_of(handle)?;
    tmux(&["kill-pane", "-t", &p]).map(|_| ())
}

fn lines_of(s: &str) -> Vec<String> {
    let mut v: Vec<String> = s.lines().map(|l| l.trim_end().to_string()).collect();
    while v.last().is_some_and(|l| l.is_empty()) {
        v.pop();
    }
    v
}

/// The rendered screen (what the TUI shows now), trailing blank rows dropped.
pub fn screen(handle: &str) -> Res<Vec<String>> {
    let p = pane_of(handle)?;
    Ok(lines_of(&tmux(&["capture-pane", "-p", "-J", "-t", &p])?))
}

/// The rendered screen with its colours (SGR escapes kept), for the dashboard's terminal view.
pub fn screen_ansi(handle: &str) -> Res<String> {
    let p = pane_of(handle)?;
    tmux(&["capture-pane", "-p", "-e", "-t", &p])
}

/// Last `n` rendered lines including scrollback, with the read source (always "screen").
pub fn tail_n(handle: &str, n: usize) -> Res<(String, Vec<String>)> {
    let p = pane_of(handle)?;
    let start = format!("-{n}");
    let mut v = lines_of(&tmux(&["capture-pane", "-p", "-J", "-S", &start, "-t", &p])?);
    if v.len() > n {
        v.drain(..v.len() - n);
    }
    Ok(("screen".into(), v))
}

pub fn tail(handle: &str) -> Res<(String, Vec<String>)> {
    tail_n(handle, 30)
}

fn type_line(pane: &str, text: &str) -> Res<()> {
    tmux(&["send-keys", "-t", pane, "-l", "--", text])?;
    // a separate Enter: TUIs that time keystrokes would read text+CR in one burst as a paste
    std::thread::sleep(std::time::Duration::from_millis(60));
    tmux(&["send-keys", "-t", pane, "Enter"]).map(|_| ())
}

fn receipt(handle: &str, what: &str) -> Value {
    json!({"handle": handle, "sent": what, "at": now_ms()})
}

/// Send exactly one line followed by Enter.
pub fn send_line(handle: &str, text: &str) -> Res<Value> {
    let p = pane_of(handle)?;
    let one: String = text.replace(['\r', '\n'], " ");
    type_line(&p, &one)?;
    Ok(receipt(handle, "line"))
}

/// Multi-line prompt as one bracketed paste followed by Enter.
pub fn send_paste(handle: &str, text: &str) -> Res<Value> {
    let p = pane_of(handle)?;
    let buf = format!("adash-{}", now_ms());
    let mut child = Command::new(&*TMUX)
        .args(["-L", SOCKET, "load-buffer", "-b", &buf, "-"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("tmux load-buffer: {e}"))?;
    {
        use std::io::Write;
        child.stdin.take().ok_or("tmux load-buffer: no stdin")?.write_all(text.as_bytes()).map_err(|e| format!("tmux load-buffer: {e}"))?;
    }
    if !child.wait().map_err(|e| e.to_string())?.success() {
        return Err("tmux load-buffer failed".into());
    }
    // -p: bracketed paste when the program asked for it; -d: drop the buffer afterwards
    tmux(&["paste-buffer", "-p", "-d", "-b", &buf, "-t", &p])?;
    // a large paste takes the TUI a while to absorb; an Enter that arrives meanwhile can be read
    // as part of the paste (a newline) instead of submit (2026-11-03, a 4-line answer to omp)
    std::thread::sleep(std::time::Duration::from_millis(1000));
    tmux(&["send-keys", "-t", &p, "Enter"])?;
    Ok(receipt(handle, "paste"))
}

/// Send raw input without Enter (keys, control bytes and escape sequences pass literally).
pub fn send_text(handle: &str, text: &str) -> Res<Value> {
    let p = pane_of(handle)?;
    if !text.is_empty() {
        tmux(&["send-keys", "-t", &p, "-l", "--", text])?;
    }
    Ok(receipt(handle, "text"))
}

/// Named tmux keys (`Enter`, `C-u`, `BSpace`, …), one send-keys call.
pub fn send_keys(handle: &str, keys: &[&str]) -> Res<Value> {
    let p = pane_of(handle)?;
    let mut args = vec!["send-keys", "-t", p.as_str()];
    args.extend_from_slice(keys);
    tmux(&args)?;
    Ok(receipt(handle, "keys"))
}

/// `n` raw DEL bytes (0x7f), in chunks. With tmux `extended-keys` on, the `BSpace` key name is
/// encoded so omp does not delete with it; the raw byte always works.
pub fn send_dels(handle: &str, n: usize) -> Res<Value> {
    let p = pane_of(handle)?;
    let mut left = n;
    while left > 0 {
        let k = left.min(200);
        let mut args = vec!["send-keys", "-t", p.as_str(), "-H"];
        args.extend(std::iter::repeat_n("7f", k));
        tmux(&args)?;
        left -= k;
    }
    Ok(receipt(handle, "del"))
}

/// A bare Enter.
pub fn send_enter(handle: &str) -> Res<Value> {
    let p = pane_of(handle)?;
    tmux(&["send-keys", "-t", &p, "Enter"])?;
    Ok(receipt(handle, "enter"))
}

/// Interrupt the running turn: omp/claude/codex stop on Esc, other programs get Ctrl+C.
pub fn interrupt(handle: &str, agent_type: &str) -> Res<Value> {
    let p = pane_of(handle)?;
    let key = if matches!(agent_type, "omp" | "claude" | "codex") { "Escape" } else { "C-c" };
    tmux(&["send-keys", "-t", &p, key])?;
    Ok(receipt(handle, key))
}

/// Name the terminal's window (the tmux status bar shows it).
pub fn set_title(handle: &str, title: &str) -> Res<Value> {
    let p = pane_of(handle)?;
    tmux(&["rename-window", "-t", &p, title])?;
    Ok(receipt(handle, "title"))
}

/// Show the terminal: select its window, then move an attached client there, else open a
/// terminal emulator attached to it (`$ADASH_TERMINAL`, default ghostty).
pub fn focus(handle: &str) -> Res<Value> {
    let ps = panes()?;
    let p = ps.iter().find(|p| p.handle == handle).ok_or_else(|| format!("terminal_not_found: {handle}"))?;
    tmux(&["select-window", "-t", &p.pane_id])?;
    tmux(&["select-pane", "-t", &p.pane_id])?;
    let clients = tmux(&["list-clients", "-F", "#{client_name}"]).unwrap_or_default();
    if let Some(c) = clients.lines().map(str::trim).find(|c| !c.is_empty()) {
        tmux(&["switch-client", "-c", c, "-t", &p.pane_id])?;
        return Ok(json!({"handle": handle, "client": c, "opened": false}));
    }
    let target = format!("={}", p.session);
    let argv: Vec<String> = vec![TMUX.to_string_lossy().into_owned(), "-L".into(), SOCKET.into(), "attach-session".into(), "-t".into(), target.clone()];
    let copy = format!("tmux -L {SOCKET} attach -t '{target}'");
    // no emulator, or it failed to start: hand the attach command to the user
    let manual = |why: String| json!({"handle": handle, "opened": false, "command": copy, "reason": why});
    let Some(emu) = pick_emulator() else {
        return Ok(manual("no terminal emulator found (set terminal.emulator in config.toml or $ADASH_TERMINAL)".into()));
    };
    let (prog, args) = emulator_command(&emu, &argv);
    let args: Vec<std::ffi::OsString> = args.iter().map(Into::into).collect();
    match crate::platform::spawn_app(&prog, &args) {
        Ok(_) => Ok(json!({"handle": handle, "opened": true, "terminal": emu})),
        Err(e) => Ok(manual(format!("{emu}: {e}"))),
    }
}

/// Linux emulators tried in order when `terminal.emulator` is `auto`.
const LINUX_EMULATORS: &[&str] =
    &["x-terminal-emulator", "ghostty", "kitty", "wezterm", "alacritty", "foot", "konsole", "gnome-terminal", "xfce4-terminal", "xterm"];

/// The emulator for "open terminal": `$ADASH_TERMINAL`, `terminal.emulator` (unless `auto`),
/// `$TERMINAL`, then the first installed one (Linux list above; macOS Ghostty, iTerm, Terminal).
pub fn pick_emulator() -> Option<String> {
    let set = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && s != "auto");
    if let Some(e) = set(std::env::var("ADASH_TERMINAL").ok())
        .or_else(|| set(Some(crate::config::get().terminal.emulator.clone())))
        .or_else(|| set(std::env::var("TERMINAL").ok()))
    {
        return Some(e);
    }
    if cfg!(target_os = "macos") {
        for (app, name) in [("/Applications/Ghostty.app", "Ghostty"), ("/Applications/iTerm.app", "iTerm")] {
            if Path::new(app).exists() {
                return Some(name.into());
            }
        }
        return Some("Terminal".into());
    }
    LINUX_EMULATORS.iter().find(|e| crate::platform::which(e)).map(|e| e.to_string())
}

/// Program and arguments that open `emu` running `argv`. A configured value may carry its own
/// leading arguments ("kitty --single-instance"); known emulators get their own command form.
pub fn emulator_command(emu: &str, argv: &[String]) -> (String, Vec<String>) {
    let mut words = emu.split_whitespace().map(String::from);
    let prog = words.next().unwrap_or_default();
    let mut args: Vec<String> = words.collect();
    let base = Path::new(&prog).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let quoted =
        || argv.iter().map(|a| if a.contains([' ', '\'', '=']) { format!("'{}'", a.replace('\'', "'\\''")) } else { a.clone() }).collect::<Vec<_>>().join(" ");
    match base.as_str() {
        // macOS apps: a new window running the command
        "Terminal" | "iTerm" if cfg!(target_os = "macos") || args.is_empty() => {
            let line = quoted().replace('\\', "\\\\").replace('"', "\\\"");
            let script = if base == "iTerm" {
                format!("tell application \"iTerm\" to create window with default profile command \"{line}\"")
            } else {
                format!("tell application \"Terminal\" to do script \"{line}\"")
            };
            ("osascript".into(), vec!["-e".into(), script])
        }
        "Ghostty" => {
            let mut a = vec!["-na".into(), "Ghostty".into(), "--args".into(), "-e".into()];
            a.extend(argv.iter().cloned());
            ("open".into(), a)
        }
        "gnome-terminal" => {
            args.push("--".into());
            args.extend(argv.iter().cloned());
            (prog, args)
        }
        "wezterm" => {
            args.extend(["start".into(), "--".into()]);
            args.extend(argv.iter().cloned());
            (prog, args)
        }
        // the command as plain arguments
        "kitty" | "foot" => {
            args.extend(argv.iter().cloned());
            (prog, args)
        }
        // -x: the rest of the line is the command (-e takes one string)
        "xfce4-terminal" => {
            args.push("-x".into());
            args.extend(argv.iter().cloned());
            (prog, args)
        }
        // ghostty, alacritty, konsole, xterm, x-terminal-emulator and unknown ones
        _ => {
            args.push("-e".into());
            args.extend(argv.iter().cloned());
            (prog, args)
        }
    }
}

/// Save the terminal's screen and scrollback (last 2000 lines) under
/// `~/.local/share/adash/screens/<handle>/<ms>-<reason>.txt`, for later diagnosis.
pub fn snapshot(handle: &str, reason: &str) -> Option<PathBuf> {
    let p = pane_of(handle).ok()?;
    let text = tmux(&["capture-pane", "-p", "-J", "-S", "-2000", "-t", &p]).ok()?;
    let safe: String = reason.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let path = screens_dir().join(handle).join(format!("{}-{safe}.txt", now_ms()));
    crate::util::atomic_write(&path, text.as_bytes()).ok()?;
    Some(path)
}

pub fn screens_dir() -> PathBuf {
    crate::platform::data_dir().join("screens")
}

/// One sender at a time per terminal: chat, relays, magi, answers and draft clearing must not
/// interleave keystrokes in the same pane.
static SEND_LOCKS: LazyLock<std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<std::sync::Mutex<()>>>>> = LazyLock::new(Default::default);

pub fn send_lock(handle: &str) -> std::sync::Arc<std::sync::Mutex<()>> {
    SEND_LOCKS.plock().entry(handle.to_string()).or_default().clone()
}

/// The handle of the terminal this process runs in.
pub fn env_handle() -> Option<String> {
    std::env::var(HANDLE_ENV).ok().filter(|h| !h.is_empty())
}

#[cfg(test)]
mod tests {
    #[test]
    fn session_names_avoid_tmux_separators() {
        assert_eq!(super::session_name("a.b:c d"), "a_b_c_d");
        assert_eq!(super::session_name(""), "adash");
    }

    #[test]
    fn trailing_blank_rows_are_dropped_but_inner_ones_kept() {
        assert_eq!(super::lines_of("a  \n\nb\n\n\n"), vec!["a", "", "b"]);
    }

    #[test]
    fn emulator_command_forms() {
        let argv: Vec<String> = ["tmux", "-L", "adash", "attach-session", "-t", "=galaxy"].iter().map(|s| s.to_string()).collect();
        let f = |e: &str| {
            let (p, a) = super::emulator_command(e, &argv);
            format!("{p} {}", a.join(" "))
        };
        assert_eq!(f("ghostty"), "ghostty -e tmux -L adash attach-session -t =galaxy");
        assert_eq!(f("gnome-terminal"), "gnome-terminal -- tmux -L adash attach-session -t =galaxy");
        assert_eq!(f("wezterm"), "wezterm start -- tmux -L adash attach-session -t =galaxy");
        assert_eq!(f("kitty --single-instance"), "kitty --single-instance tmux -L adash attach-session -t =galaxy");
        assert_eq!(f("xfce4-terminal"), "xfce4-terminal -x tmux -L adash attach-session -t =galaxy");
        assert_eq!(f("/usr/bin/alacritty"), "/usr/bin/alacritty -e tmux -L adash attach-session -t =galaxy");
        let (p, a) = super::emulator_command("Terminal", &argv);
        assert_eq!(p, "osascript");
        assert!(a[1].contains("do script \"tmux -L adash attach-session -t '=galaxy'\""), "{}", a[1]);
    }

    #[test]
    fn harness_notifications_off_only_for_known_switches() {
        assert_eq!(super::silence_harness("claude --resume 'x'"), r#"claude --settings '{"preferredNotifChannel":"notifications_disabled"}' --resume 'x'"#);
        assert_eq!(super::silence_harness("codex resume 'id'"), "codex -c tui.notifications=false resume 'id'");
        assert_eq!(super::silence_harness("codex"), "codex -c tui.notifications=false");
        // already there (resume keeps -c flags): unchanged
        let c = "codex -c tui.notifications=false resume 'id'";
        assert_eq!(super::silence_harness(c), c);
        // omp goes by environment; other commands untouched
        assert_eq!(super::silence_harness("omp --resume x"), "omp --resume x");
        assert_eq!(super::silence_harness("agy"), "agy");
    }
}
