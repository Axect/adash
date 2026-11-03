//! Everything that differs between Linux and macOS: directories, process inspection,
//! notifications, opening files, and the per-user service manager.
//!
//! Linux reads `/proc`; macOS uses `sysctl(KERN_PROCARGS2)` for arguments and environment,
//! `ps` for parents and start times, and `lsof` for the working directory and open files.
use crate::util::{home, Res};
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------------------------
// directories

fn xdg(var: &str) -> Option<PathBuf> {
    std::env::var(var).ok().filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// macOS keeps config, data and state together under Application Support unless an XDG
/// variable says otherwise.
#[cfg(target_os = "macos")]
fn mac_support() -> PathBuf {
    home().join("Library/Application Support/adash")
}

/// `config.toml`, `meta.yaml`, `tmux.conf`.
pub fn config_dir() -> PathBuf {
    if let Some(d) = xdg("XDG_CONFIG_HOME") {
        return d.join("adash");
    }
    #[cfg(target_os = "macos")]
    return mac_support();
    #[cfg(not(target_os = "macos"))]
    home().join(".config/adash")
}

/// Analytics store, threads, uploads, reports, devices.
pub fn data_dir() -> PathBuf {
    if let Some(d) = xdg("XDG_DATA_HOME") {
        return d.join("adash");
    }
    #[cfg(target_os = "macos")]
    return mac_support();
    #[cfg(not(target_os = "macos"))]
    home().join(".local/share/adash")
}

/// Registry, history, pins.
pub fn state_dir() -> PathBuf {
    if let Some(d) = xdg("XDG_STATE_HOME") {
        return d.join("adash");
    }
    #[cfg(target_os = "macos")]
    return mac_support().join("state");
    #[cfg(not(target_os = "macos"))]
    home().join(".local/state/adash")
}

// ---------------------------------------------------------------------------------------------
// processes

/// All process ids visible to this user.
pub fn pids() -> Vec<u32> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_dir("/proc").map(|rd| rd.flatten().filter_map(|e| e.file_name().to_string_lossy().parse().ok()).collect()).unwrap_or_default()
    }
    #[cfg(not(target_os = "linux"))]
    {
        ps_table().into_iter().map(|p| p.0).collect()
    }
}

/// Command line arguments.
pub fn args(pid: u32) -> Option<Vec<String>> {
    #[cfg(target_os = "linux")]
    {
        let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        Some(raw.split(|b| *b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect())
    }
    #[cfg(target_os = "macos")]
    {
        mac::procargs(pid).map(|(a, _)| a)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

/// One environment variable of a process.
pub fn env_var(pid: u32, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    #[cfg(target_os = "linux")]
    {
        let env = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
        env.split(|b| *b == 0).find(|e| e.starts_with(prefix.as_bytes())).map(|e| String::from_utf8_lossy(&e[prefix.len()..]).into_owned())
    }
    #[cfg(target_os = "macos")]
    {
        mac::procargs(pid)?.1.into_iter().find_map(|e| e.strip_prefix(&prefix).map(String::from))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (pid, prefix);
        None
    }
}

/// Working directory.
pub fn cwd(pid: u32) -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        lsof(pid, true).into_iter().next()
    }
}

/// Paths of the files a process has open.
pub fn open_files(pid: u32) -> Vec<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_dir(format!("/proc/{pid}/fd")).map(|rd| rd.flatten().filter_map(|e| std::fs::read_link(e.path()).ok()).collect()).unwrap_or_default()
    }
    #[cfg(not(target_os = "linux"))]
    {
        lsof(pid, false)
    }
}

/// Parent process id.
pub fn ppid(pid: u32) -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        stat.rsplit_once(')')?.1.split_whitespace().nth(1)?.parse().ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        ps_table().into_iter().find(|p| p.0 == pid).map(|p| p.1)
    }
}

/// Process start time (ms since the epoch; 0 when unknown).
pub fn start_ms(pid: u32) -> i64 {
    #[cfg(target_os = "linux")]
    {
        static BOOT: std::sync::LazyLock<i64> = std::sync::LazyLock::new(|| {
            std::fs::read_to_string("/proc/stat")
                .ok()
                .and_then(|s| s.lines().find(|l| l.starts_with("btime ")).and_then(|l| l[6..].trim().parse::<i64>().ok()))
                .unwrap_or(0)
                * 1000
        });
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let after = stat.rsplit_once(')').map(|x| x.1).unwrap_or("");
        // fields after ')' start at field 3; starttime is field 22 → index 19 (CLK_TCK = 100)
        let ticks: i64 = after.split_whitespace().nth(19).and_then(|t| t.parse().ok()).unwrap_or(0);
        *BOOT + ticks * 10
    }
    #[cfg(not(target_os = "linux"))]
    {
        ps_table().into_iter().find(|p| p.0 == pid).map(|p| p.2).unwrap_or(0)
    }
}

pub fn alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Command::new("kill").args(["-0", &pid.to_string()]).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success())
    }
}

/// This process's user id.
pub fn my_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    // a file we just created is ours on every unix
    let p = std::env::temp_dir().join(format!("adash-uid-{}", std::process::id()));
    let uid = std::fs::write(&p, b"").ok().and_then(|_| std::fs::metadata(&p).ok()).map(|m| m.uid()).unwrap_or(u32::MAX);
    let _ = std::fs::remove_file(&p);
    uid
}

/// (pid, ppid, start ms) of every process, from `ps` (cached for one second).
#[cfg(not(target_os = "linux"))]
fn ps_table() -> Vec<(u32, u32, i64)> {
    use std::sync::{LazyLock, Mutex};
    static CACHE: LazyLock<Mutex<(i64, Vec<(u32, u32, i64)>)>> = LazyLock::new(Default::default);
    let now = crate::util::now_ms();
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if now - c.0 < 1000 && !c.1.is_empty() {
        return c.1.clone();
    }
    // etime is [[dd-]hh:]mm:ss; start = now - elapsed (second resolution)
    let out = Command::new("ps").args(["-axo", "pid=,ppid=,etime="]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let v: Vec<(u32, u32, i64)> = out
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            Some((pid, ppid, now - elapsed_ms(it.next()?)?))
        })
        .collect();
    *c = (now, v.clone());
    v
}

/// `ps` etime "[[dd-]hh:]mm:ss" → ms.
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn elapsed_ms(s: &str) -> Option<i64> {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<i64>().ok()?, r),
        None => (0, s),
    };
    let parts: Vec<i64> = rest.split(':').map(|x| x.parse().ok()).collect::<Option<_>>()?;
    let secs = match parts.as_slice() {
        [m, s] => m * 60 + s,
        [h, m, s] => h * 3600 + m * 60 + s,
        _ => return None,
    };
    Some((days * 86_400 + secs) * 1000)
}

/// `lsof -Fn` names of a process's cwd (`cwd_only`) or of its regular open files.
#[cfg(not(target_os = "linux"))]
fn lsof(pid: u32, cwd_only: bool) -> Vec<PathBuf> {
    let p = pid.to_string();
    let mut args = vec!["-a", "-p", p.as_str(), "-Fn", "-w"];
    if cwd_only {
        args.extend(["-d", "cwd"]);
    }
    let out = Command::new("lsof").args(&args).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    out.lines().filter_map(|l| l.strip_prefix('n')).filter(|n| n.starts_with('/')).map(PathBuf::from).collect()
}

#[cfg(target_os = "macos")]
mod mac {
    /// `sysctl(KERN_PROCARGS2)`: argc, the executable path, then argv and envp as NUL-separated
    /// strings. Only processes of the same user are readable.
    pub fn procargs(pid: u32) -> Option<(Vec<String>, Vec<String>)> {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
        let mut size: libc::size_t = 0;
        // SAFETY: size query with a null buffer, then a buffer of that size
        unsafe {
            if libc::sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) != 0 || size == 0 {
                return None;
            }
        }
        let mut buf = vec![0u8; size];
        unsafe {
            if libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr() as *mut libc::c_void, &mut size, std::ptr::null_mut(), 0) != 0 {
                return None;
            }
        }
        buf.truncate(size);
        if buf.len() < 4 {
            return None;
        }
        let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        let mut rest = &buf[4..];
        // executable path, then NUL padding
        let end = rest.iter().position(|b| *b == 0)?;
        rest = &rest[end..];
        let start = rest.iter().position(|b| *b != 0)?;
        rest = &rest[start..];
        let mut strings = rest.split(|b| *b == 0).map(|s| String::from_utf8_lossy(s).into_owned());
        let args: Vec<String> = strings.by_ref().take(argc).collect();
        let env: Vec<String> = strings.take_while(|s| !s.is_empty()).collect();
        Some((args, env))
    }
}

/// A command that runs `program` with a deadline: coreutils `timeout` (Linux, or `gtimeout`
/// on macOS with coreutils), else `perl -e 'alarm …; exec'` (perl ships with macOS). Append
/// the program and its arguments. A timed-out run exits 124 (timeout) or by SIGALRM (perl).
pub fn timeout(secs: u64) -> Command {
    for t in ["timeout", "gtimeout"] {
        if cfg!(target_os = "linux") && t == "timeout" || which(t) {
            let mut c = Command::new(t);
            c.args(["-k", "2", &secs.to_string()]);
            return c;
        }
    }
    let mut c = Command::new("perl");
    c.args(["-e", "alarm shift; exec @ARGV or die $!", &secs.to_string()]);
    c
}

/// Did a run started with [`timeout`] hit its deadline?
pub fn timed_out(st: &std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt;
    matches!(st.code(), Some(124 | 137)) || st.signal().is_some_and(|s| s == 14 || s == 9)
}

// ---------------------------------------------------------------------------------------------
// desktop

/// The program that opens a file with its default application.
pub fn opener() -> &'static str {
    if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    }
}

/// Start a desktop app (viewer, editor, terminal) that must outlive the adash service: on Linux
/// in its own transient systemd scope (`systemd-run --user --scope`), since the service's
/// `KillMode=control-group` stops everything in its cgroup on restart and `setsid` does not
/// leave the cgroup; elsewhere, or without systemd-run, a new session. The child is reaped in
/// the background.
pub fn spawn_app(prog: &str, args: &[std::ffi::OsString]) -> std::io::Result<()> {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let detached = |mut c: Command| -> std::io::Result<std::process::Child> {
        c.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                c.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
        }
        c.spawn()
    };
    let child = if cfg!(target_os = "linux") && which("systemd-run") && std::env::var_os("XDG_RUNTIME_DIR").is_some() {
        let unit = format!("adash-open-{}-{}", crate::util::now_ms(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let mut c = Command::new("systemd-run");
        c.args(["--user", "--scope", "--collect", "--quiet", "--unit", &unit, "--"]).arg(prog).args(args);
        detached(c).or_else(|_| {
            let mut d = Command::new(prog);
            d.args(args);
            detached(d)
        })?
    } else {
        let mut d = Command::new(prog);
        d.args(args);
        detached(d)?
    };
    let mut child = child;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// Open `path` on the desktop, detached; Markdown with the configured app when set.
pub fn open_path(path: &Path, markdown_app: Option<&str>) -> Res<()> {
    let is_md = path.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"));
    let (prog, mut args): (String, Vec<std::ffi::OsString>) = match (is_md, markdown_app.filter(|a| !a.is_empty())) {
        (true, Some(app)) if cfg!(target_os = "macos") && !app.starts_with('/') => ("open".into(), vec!["-a".into(), app.into()]),
        (true, Some(app)) => (app.to_string(), vec![]),
        _ => (opener().to_string(), vec![]),
    };
    args.push(path.as_os_str().to_owned());
    spawn_app(&prog, &args).map_err(|e| format!("open {}: {e}", path.display()))
}

/// Notification backend: config value, else the platform default that is installed.
fn notify_backend() -> String {
    let b = crate::config::get().notifications.backend.clone();
    if b != "auto" {
        return b;
    }
    if cfg!(target_os = "macos") {
        if which("terminal-notifier") {
            "terminal-notifier".into()
        } else {
            "osascript".into()
        }
    } else if which("notify-send") {
        "notify-send".into()
    } else {
        "none".into()
    }
}

pub fn which(bin: &str) -> bool {
    std::env::var("PATH").unwrap_or_default().split(':').any(|d| Path::new(d).join(bin).is_file())
}

/// Show a desktop notification. With an action label, block until it is dismissed (≤15 min)
/// and return whether the action was clicked (notify-send only; others return false at once).
/// `tag` groups notifications: a new one with the same tag replaces the one still on screen
/// (notification servers that support the stack-tag / synchronous hints; macOS groups).
pub fn notify(summary: &str, body: &str, action: Option<&str>, tag: Option<&str>) -> bool {
    match notify_backend().as_str() {
        "notify-send" => {
            let mut a: Vec<String> = vec!["-a".into(), "adash".into()];
            if let Some(t) = tag {
                a.extend([
                    "-h".into(),
                    format!("string:x-dunst-stack-tag:adash-{t}"),
                    "-h".into(),
                    format!("string:x-canonical-private-synchronous:adash-{t}"),
                ]);
            }
            match action {
                Some(label) => {
                    a.extend([format!("--action=open={label}"), summary.into(), body.into()]);
                    timeout(900)
                        .args(std::iter::once("notify-send".to_string()).chain(a))
                        .output()
                        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "open")
                }
                None => {
                    a.extend([summary.into(), body.into()]);
                    let _ = Command::new("notify-send").args(a).spawn().map(|mut c| std::thread::spawn(move || c.wait()));
                    false
                }
            }
        }
        "terminal-notifier" => {
            let _ = Command::new("terminal-notifier")
                .args(["-title", "adash", "-subtitle", summary, "-message", body, "-group", &format!("adash-{}", tag.unwrap_or(""))])
                .spawn()
                .map(|mut c| std::thread::spawn(move || c.wait()));
            false
        }
        "osascript" => {
            let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
            let script = format!("display notification \"{}\" with title \"adash\" subtitle \"{}\"", esc(body), esc(summary));
            let _ = Command::new("osascript").args(["-e", &script]).spawn().map(|mut c| std::thread::spawn(move || c.wait()));
            false
        }
        _ => false,
    }
}

/// Chromium-family browsers for PDF reports (PATH names, then macOS app bundles).
pub fn chrome() -> Option<PathBuf> {
    let configured = crate::config::get().reports.chrome.clone();
    if let Some(c) = configured.filter(|c| !c.is_empty()) {
        return Some(PathBuf::from(c));
    }
    for b in ["chromium", "chromium-browser", "google-chrome-stable", "google-chrome", "chrome"] {
        for d in std::env::var("PATH").unwrap_or_default().split(':') {
            let p = Path::new(d).join(b);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "/Applications/Chromium.app/Contents/MacOS/Chromium"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

// ---------------------------------------------------------------------------------------------
// per-user services (systemd on Linux, launchd on macOS)

/// Install and start a long-running user service. `keep_running`: restart when it exits.
pub fn install_service(name: &str, description: &str, program: &[String], path_env: &str, after: Option<&str>) -> Res<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let _ = after;
        let label = format!("dev.adash.{name}");
        let plist = home().join("Library/LaunchAgents").join(format!("{label}.plist"));
        let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let args: String = program.iter().map(|a| format!("    <string>{}</string>\n", esc(a))).collect();
        let logs = data_dir().join("logs");
        let _ = std::fs::create_dir_all(&logs);
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key><string>{label}</string>\n  <!-- {} -->\n  <key>ProgramArguments</key>\n  <array>\n{args}  </array>\n  <key>EnvironmentVariables</key>\n  <dict><key>PATH</key><string>{}</string></dict>\n  <key>RunAtLoad</key><true/>\n  <key>KeepAlive</key><true/>\n  <key>StandardOutPath</key><string>{}</string>\n  <key>StandardErrorPath</key><string>{}</string>\n</dict>\n</plist>\n",
            esc(description),
            esc(path_env),
            esc(&logs.join(format!("{name}.log")).to_string_lossy()),
            esc(&logs.join(format!("{name}.log")).to_string_lossy()),
        );
        crate::util::atomic_write(&plist, body.as_bytes())?;
        let domain = format!("gui/{}", my_uid());
        // reload: bootout may fail when it was not loaded
        let _ = Command::new("launchctl").args(["bootout", &format!("{domain}/{label}")]).status();
        let st = Command::new("launchctl").args(["bootstrap", &domain]).arg(&plist).status().map_err(|e| e.to_string())?;
        if !st.success() {
            return Err(format!("launchctl bootstrap {domain} {} failed", plist.display()));
        }
        Ok(plist)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut env = format!("Environment=PATH={path_env}\n");
        for k in ["DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "XDG_RUNTIME_DIR"] {
            if let Ok(v) = std::env::var(k) {
                env.push_str(&format!("Environment={k}={v}\n"));
            }
        }
        let quoted: Vec<String> = program.iter().map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.clone() }).collect();
        let unit = format!(
            "[Unit]\nDescription={description}\n{}\n[Service]\nExecStart={}\n{env}Restart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n",
            after.map(|u| format!("Wants={u}\nAfter=graphical-session.target {u}\n")).unwrap_or_default(),
            quoted.join(" ")
        );
        let p = home().join(".config/systemd/user").join(format!("{name}.service"));
        crate::util::atomic_write(&p, unit.as_bytes())?;
        for args in [vec!["--user", "daemon-reload"], vec!["--user", "enable", &format!("{name}.service")]] {
            let st = Command::new("systemctl").args(&args).status().map_err(|e| e.to_string())?;
            if !st.success() {
                return Err(format!("systemctl {} failed", args.join(" ")));
            }
        }
        Ok(p)
    }
}

/// Start a user service (no-op when running). `restart`: restart it when it runs.
pub fn start_service(name: &str, restart: bool) -> Res<()> {
    #[cfg(target_os = "macos")]
    {
        let target = format!("gui/{}/dev.adash.{name}", my_uid());
        let mut args = vec!["kickstart"];
        if restart {
            args.push("-k");
        }
        args.push(&target);
        let st = Command::new("launchctl").args(&args).status().map_err(|e| format!("launchctl kickstart {target}: {e}"))?;
        if !st.success() {
            return Err(format!("launchctl kickstart {target} failed (run `adash install-service`)"));
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let unit = format!("{name}.service");
        let st = Command::new("systemctl")
            .args(["--user", if restart { "restart" } else { "start" }, &unit])
            .status()
            .map_err(|e| format!("systemctl start {unit}: {e}"))?;
        if !st.success() {
            return Err(format!("systemctl --user start {unit} failed (run `adash install-service`)"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn this_process_is_visible() {
        let me = std::process::id();
        assert!(super::alive(me));
        assert!(super::pids().contains(&me));
        assert!(super::args(me).is_some_and(|a| !a.is_empty()));
        assert!(super::cwd(me).is_some());
        assert!(super::start_ms(me) > 0);
        assert!(super::ppid(me).is_some());
    }

    #[test]
    fn ps_elapsed_formats() {
        assert_eq!(super::elapsed_ms("05:07"), Some(307_000));
        assert_eq!(super::elapsed_ms("02:05:07"), Some(7_507_000));
        assert_eq!(super::elapsed_ms("3-02:05:07"), Some((3 * 86_400 + 7_507) * 1000));
        assert_eq!(super::elapsed_ms("x"), None);
    }
}
