//! Machine configuration: `config.toml` in the config directory (Linux
//! `$XDG_CONFIG_HOME/adash` or `~/.config/adash`; macOS `~/Library/Application Support/adash`
//! unless `XDG_CONFIG_HOME` is set). Every key is optional. `adash config init` writes a
//! commented default; `adash config path` prints where it is read from.
//!
//! Project data (projects, human tasks, cache TTL table, phone logins) stays in
//! `meta.yaml`, which the dashboard edits.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::LazyLock;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// HTTP port (env `ADASH_PORT` wins).
    pub port: u16,
    /// Agent harnesses adash looks for; a disabled one is never matched to a terminal.
    pub harnesses: BTreeMap<String, bool>,
    pub terminal: Terminal,
    pub notifications: Notifications,
    pub open: Open,
    pub reports: Reports,
    pub magi: Magi,
    pub network: Network,
    pub mobile: Mobile,
    pub launch: Launch,
    pub restart: Restart,
    pub theme: ThemeDefaults,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Restart {
    /// restart idle agents on the same session as soon as their harness was updated
    pub auto_outdated: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Launch {
    /// Agents the dashboard starts accept their harness's "trust this folder?" screen (you chose
    /// the folder); per launch it can be turned off.
    pub auto_trust: bool,
}

impl Default for Launch {
    fn default() -> Self {
        Launch { auto_trust: true }
    }
}

/// Themes a device uses until it picks its own (per device, in the browser).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ThemeDefaults {
    /// unset: dark (adash dark)
    pub dark: Option<String>,
    /// unset: catppuccin-latte
    pub light: Option<String>,
}

impl ThemeDefaults {
    pub fn dark(&self) -> &str {
        self.dark.as_deref().unwrap_or("dark")
    }
    pub fn light(&self) -> &str {
        self.light.as_deref().unwrap_or("catppuccin-latte")
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Terminal {
    /// Emulator opened by "open terminal" when no tmux client is attached (env `ADASH_TERMINAL`
    /// wins). `auto`: `$TERMINAL`, then the first installed of x-terminal-emulator, ghostty,
    /// kitty, wezterm, alacritty, foot, konsole, gnome-terminal, xfce4-terminal, xterm (macOS:
    /// Ghostty, iTerm, Terminal). A value may carry extra arguments ("kitty --single-instance").
    pub emulator: String,
    /// tmux binary.
    pub tmux: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Notifications {
    /// auto | notify-send | terminal-notifier | osascript | none
    pub backend: String,
    /// turn off the agents' own desktop notifications in adash terminals (adash notifies once)
    pub silence_harness: bool,
    /// minutes a working agent may show no change (session, screen, CPU) before "응답 없음"; 0 = off
    pub stall_minutes: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Open {
    /// App for Markdown files opened from chat (e.g. `typora`; macOS: an app name such as
    /// `Typora`). Unset: the system default (`xdg-open` / `open`).
    pub markdown_app: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Reports {
    /// Chromium-family browser for PDF reports. Unset: searched on PATH (and in /Applications).
    pub chrome: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Magi {
    /// SKILL.md of a multi-agent review skill named in magi instructions. Unset: the magi
    /// button and `POST /api/magi` are off.
    pub skill: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Network {
    /// Probe provider reachability and hold adash-initiated sends while unreachable.
    pub enabled: bool,
    /// provider → URL probed (any HTTP answer after TLS = reachable).
    pub probes: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Mobile {
    /// Accept requests that arrive through `tailscale serve` (phone access with device approval).
    pub tailscale: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            port: crate::util::DEFAULT_PORT,
            harnesses: ["omp", "claude", "codex", "antigravity"].iter().map(|h| (h.to_string(), true)).collect(),
            terminal: Terminal::default(),
            notifications: Notifications::default(),
            open: Open::default(),
            reports: Reports::default(),
            magi: Magi::default(),
            network: Network::default(),
            mobile: Mobile::default(),
            launch: Launch::default(),
            restart: Restart::default(),
            theme: ThemeDefaults::default(),
        }
    }
}

impl Default for Terminal {
    fn default() -> Self {
        Terminal { emulator: "auto".into(), tmux: "tmux".into() }
    }
}

impl Default for Notifications {
    fn default() -> Self {
        Notifications { backend: "auto".into(), silence_harness: true, stall_minutes: 10 }
    }
}

impl Default for Network {
    fn default() -> Self {
        Network { enabled: true, probes: crate::network::PROVIDERS.iter().map(|(p, u)| (p.to_string(), u.to_string())).collect() }
    }
}

impl Default for Mobile {
    fn default() -> Self {
        Mobile { tailscale: true }
    }
}

impl Config {
    pub fn harness_enabled(&self, kind: &str) -> bool {
        self.harnesses.get(kind).copied().unwrap_or(true)
    }
}

pub fn path() -> PathBuf {
    crate::platform::config_dir().join("config.toml")
}

/// `config.yaml` → `config.toml` in `dir` (commented, same values); the old file is kept as
/// `config.yaml.migrated`. `Ok(None)`: nothing to convert (no YAML, or TOML already there).
/// A YAML that does not parse is an error and nothing is written.
pub fn migrate_yaml_in(dir: &std::path::Path) -> Result<Option<Config>, String> {
    let (old, new) = (dir.join("config.yaml"), dir.join("config.toml"));
    if new.exists() || !old.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&old).map_err(|e| format!("{}: {e}", old.display()))?;
    let c: Config = serde_yaml::from_str(&text).map_err(|e| format!("{} does not parse ({e}); fix it or remove it, then retry", old.display()))?;
    let out = config_text(&c);
    // the TOML must read back as the same settings before the YAML is moved aside
    let back: Config = toml::from_str(&out).map_err(|e| format!("converted config does not parse ({e}); {} left unchanged", old.display()))?;
    if !same(&c, &back) {
        return Err(format!("converted config differs from {}; left unchanged", old.display()));
    }
    crate::util::atomic_write(&new, out.as_bytes()).map_err(|e| format!("writing {}: {e}", new.display()))?;
    let kept = old.with_file_name("config.yaml.migrated");
    std::fs::rename(&old, &kept).map_err(|e| format!("keeping {} as {}: {e}", old.display(), kept.display()))?;
    eprintln!("adash: converted {} to {} (old file kept as {})", old.display(), new.display(), kept.display());
    Ok(Some(c))
}

/// Same effective settings (an omitted harness means enabled).
fn same(a: &Config, b: &Config) -> bool {
    let norm = |c: &Config| {
        let mut v = serde_json::to_value(c).unwrap_or_default();
        v["harnesses"] = serde_json::json!(["omp", "claude", "codex", "antigravity"].map(|h| c.harness_enabled(h)));
        v
    };
    norm(a) == norm(b)
}

static CONFIG: LazyLock<Config> = LazyLock::new(|| {
    if cfg!(test) {
        return Config::default();
    }
    match migrate_yaml_in(&crate::platform::config_dir()) {
        Ok(Some(c)) => return c,
        Ok(None) => {}
        Err(e) => {
            eprintln!("adash: {e}; using defaults");
            return Config::default();
        }
    }
    match std::fs::read_to_string(path()) {
        Ok(t) => toml::from_str(&t).unwrap_or_else(|e| {
            eprintln!("adash: {}: {e}; using defaults", path().display());
            Config::default()
        }),
        Err(_) => Config::default(),
    }
});

/// The configuration, read once per process.
pub fn get() -> &'static Config {
    &CONFIG
}

pub const SCHEMA_URL: &str = "https://raw.githubusercontent.com/Axect/adash/main/docs/schema/config.schema.json";

fn q(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// Commented `config.toml` holding the values of `c` (unset optional keys stay commented).
pub fn config_text(c: &Config) -> String {
    let d = Config::default();
    let opt = |key: &str, v: &Option<String>, example: &str| match v {
        Some(x) => format!("{key} = {}\n", q(x)),
        None => format!("# {key} = {}\n", q(example)),
    };
    let harnesses: String = ["omp", "claude", "codex", "antigravity"].iter().map(|h| format!("{h} = {}\n", c.harness_enabled(h))).collect();
    let probes: String = c.network.probes.iter().map(|(k, v)| format!("{} = {}\n", q(k), q(v))).collect();
    let _ = d;
    format!(
        "#:schema {SCHEMA_URL}\n\
         # adash configuration. Every key is optional; delete what you do not change.\n\
         # Project data (projects, tasks, cache TTLs, phone logins) lives in meta.yaml.\n\
         \n\
         # HTTP port of the dashboard (env ADASH_PORT wins).\n\
         port = {port}\n\
         \n\
         # Agent harnesses to recognize. A disabled harness is never matched to a terminal.\n\
         [harnesses]\n{harnesses}\
         \n\
         [terminal]\n\
         # opened by \"open terminal\" when no tmux client is attached (env ADASH_TERMINAL wins);\n\
         # auto = $TERMINAL, then the first installed of x-terminal-emulator, ghostty, kitty, wezterm,\n\
         # alacritty, foot, konsole, gnome-terminal, xfce4-terminal, xterm (macOS: Ghostty, iTerm, Terminal)\n\
         emulator = {emu}\n\
         tmux = {tmux}\n\
         \n\
         [notifications]\n\
         # auto | notify-send | terminal-notifier | osascript | none\n\
         backend = {backend}\n\
         # turn off omp / Claude Code / Codex desktop notifications in adash terminals, so each\n\
         # event notifies once (from adash); agents outside adash keep theirs\n\
         silenceHarness = {silence}\n\
         # minutes a working agent may show no change (session, screen, CPU) before it is flagged; 0 = off\n\
         stallMinutes = {stall}\n\
         \n\
         [open]\n\
         # app for Markdown files opened from chat; unset = system default\n\
         {md}\
         \n\
         [reports]\n\
         # Chromium-family browser for PDF reports; unset = search PATH\n\
         {chrome}\
         \n\
         [magi]\n\
         # SKILL.md of a multi-agent review skill; unset = magi feature off\n\
         {skill}\
         \n\
         [network]\n\
         # probe providers and hold automatic sends while they are unreachable\n\
         enabled = {net}\n\
         \n\
         [network.probes]\n{probes}\
         \n\
         [mobile]\n\
         # accept phones through `tailscale serve` (each device needs approval)\n\
         tailscale = {ts}\n\
         \n\
         [launch]\n\
         # agents started from the dashboard accept the harness's \"trust this folder?\" screen\n\
         autoTrust = {autotrust}\n\
         \n\
         [restart]\n\
         # restart idle agents (same session) as soon as their harness was updated\n\
         autoOutdated = {autoout}\n\
         \n\
         [theme]\n\
         # themes a device uses until it picks its own (`adash theme list`)\n\
         {tdark}\
         {tlight}",
        port = c.port,
        emu = q(&c.terminal.emulator),
        tmux = q(&c.terminal.tmux),
        backend = q(&c.notifications.backend),
        silence = c.notifications.silence_harness,
        stall = c.notifications.stall_minutes,
        md = opt("markdownApp", &c.open.markdown_app, "typora"),
        chrome = opt("chrome", &c.reports.chrome, "/usr/bin/chromium"),
        skill = opt("skill", &c.magi.skill, "~/skills/magi-research/SKILL.md"),
        net = c.network.enabled,
        ts = c.mobile.tailscale,
        autotrust = c.launch.auto_trust,
        autoout = c.restart.auto_outdated,
        tdark = opt("dark", &c.theme.dark, "dark"),
        tlight = opt("light", &c.theme.light, "catppuccin-latte"),
    )
}

/// Set `[theme] dark|light = name` in `config.toml`, keeping every other line (the file is
/// created from the commented template when missing). The result must parse with the other
/// settings unchanged.
pub fn set_theme_default(dark: bool, name: &str) -> Result<std::path::PathBuf, String> {
    set_theme_default_in(&path(), dark, name)
}

fn set_theme_default_in(p: &std::path::Path, dark: bool, name: &str) -> Result<std::path::PathBuf, String> {
    let p = p.to_path_buf();
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(_) => default_text(),
    };
    let before: Config = toml::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?;
    let key = if dark { "dark" } else { "light" };
    let line = format!("{key} = {}", q(name));
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let start = lines.iter().position(|l| l.trim() == "[theme]");
    match start {
        Some(i) => {
            let end = lines.iter().skip(i + 1).position(|l| l.trim_start().starts_with('[')).map_or(lines.len(), |n| i + 1 + n);
            let k = (i + 1..end).find(|&j| {
                let t = lines[j].trim_start().trim_start_matches('#').trim_start();
                t.starts_with(key) && t[key.len()..].trim_start().starts_with('=')
            });
            match k {
                Some(j) => lines[j] = line,
                None => lines.insert(i + 1, line),
            }
        }
        None => {
            lines.push(String::new());
            lines.push("[theme]".into());
            lines.push(line);
        }
    }
    let out = lines.join("\n") + "\n";
    let after: Config = toml::from_str(&out).map_err(|e| format!("edited config does not parse ({e}); left unchanged"))?;
    let mut expect = before.clone();
    if dark {
        expect.theme.dark = Some(name.to_string());
    } else {
        expect.theme.light = Some(name.to_string());
    }
    if !same(&expect, &after) {
        return Err("editing config.toml would change other settings; left unchanged".into());
    }
    crate::util::atomic_write(&p, out.as_bytes()).map_err(|e| e.to_string())?;
    Ok(p)
}

/// Commented default `config.toml`.
pub fn default_text() -> String {
    config_text(&Config::default())
}

/// JSON Schema of `config.toml`.
pub fn schema() -> serde_json::Value {
    use serde_json::json;
    let s = |d: &str| json!({"type": "string", "description": d});
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "adash config.toml",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "port": {"type": "integer", "minimum": 1, "maximum": 65535, "description": "HTTP port (env ADASH_PORT wins)"},
            "harnesses": {"type": "object", "additionalProperties": false, "properties": {"omp": {"type": "boolean"}, "claude": {"type": "boolean"}, "codex": {"type": "boolean"}, "antigravity": {"type": "boolean"}}},
            "terminal": {"type": "object", "additionalProperties": false, "properties": {"emulator": s("auto, or an emulator command such as ghostty / \"kitty --single-instance\""), "tmux": s("tmux binary")}},
            "notifications": {"type": "object", "additionalProperties": false, "properties": {"backend": {"enum": ["auto", "notify-send", "terminal-notifier", "osascript", "none"]}, "silenceHarness": {"type": "boolean"}, "stallMinutes": {"type": "integer", "minimum": 0}}},
            "open": {"type": "object", "additionalProperties": false, "properties": {"markdownApp": s("app for Markdown files opened from chat")}},
            "reports": {"type": "object", "additionalProperties": false, "properties": {"chrome": s("Chromium-family browser for PDF reports")}},
            "magi": {"type": "object", "additionalProperties": false, "properties": {"skill": s("SKILL.md of a multi-agent review skill; unset = magi off")}},
            "network": {"type": "object", "additionalProperties": false, "properties": {"enabled": {"type": "boolean"}, "probes": {"type": "object", "additionalProperties": {"type": "string", "format": "uri"}}}},
            "mobile": {"type": "object", "additionalProperties": false, "properties": {"tailscale": {"type": "boolean"}}},
            "launch": {"type": "object", "additionalProperties": false, "properties": {"autoTrust": {"type": "boolean"}}},
            "restart": {"type": "object", "additionalProperties": false, "properties": {"autoOutdated": {"type": "boolean"}}},
            "theme": {"type": "object", "additionalProperties": false, "properties": {"dark": s("theme for dark mode (default dark, adash dark)"), "light": s("theme for light mode (default catppuccin-latte)")}}
        }
    })
}

/// Expand a leading `~/`.
pub fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(r) => crate::util::home().join(r),
        None => PathBuf::from(p),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn default_text_parses_back_to_the_defaults() {
        let c: super::Config = toml::from_str(&super::default_text()).unwrap();
        assert_eq!(c.port, super::Config::default().port);
        assert!(c.harness_enabled("omp") && c.network.enabled && c.magi.skill.is_none());
        assert_eq!(c.network.probes.len(), crate::network::PROVIDERS.len());
        let partial: super::Config = toml::from_str("[harnesses]\ncodex = false\n[notifications]\nbackend = \"none\"\n").unwrap();
        assert!(!partial.harness_enabled("codex") && partial.harness_enabled("claude"));
        assert_eq!(partial.notifications.backend, "none");
    }

    #[test]
    fn yaml_values_survive_the_toml_conversion() {
        let yaml =
            "port: 7900\nterminal:\n  emulator: ghostty\nopen:\n  markdownApp: typora\nmagi:\n  skill: ~/skills/magi/SKILL.md\nharnesses:\n  codex: false\n";
        let c: super::Config = serde_yaml::from_str(yaml).unwrap();
        let back: super::Config = toml::from_str(&super::config_text(&c)).unwrap();
        assert!(super::same(&c, &back));
        assert_eq!((back.port, back.terminal.emulator.as_str(), back.open.markdown_app.as_deref()), (7900, "ghostty", Some("typora")));
        assert!(!back.harness_enabled("codex"));
    }

    #[test]
    fn shipped_schemas_match_the_code() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/schema");
        let read = |f: &str| serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(dir.join(f)).unwrap()).unwrap();
        assert_eq!(read("config.schema.json"), super::schema(), "run `adash config schema > docs/schema/config.schema.json`");
        assert_eq!(read("theme.schema.json"), crate::theme::schema(), "run `adash theme schema > docs/schema/theme.schema.json`");
    }

    #[test]
    fn yaml_migration_converts_once_and_refuses_broken_files() {
        let dir = std::env::temp_dir().join(format!("adash-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.yaml"), "terminal:\n  emulator: ghostty\n").unwrap();
        let c = super::migrate_yaml_in(&dir).unwrap().unwrap();
        assert_eq!(c.terminal.emulator, "ghostty");
        assert!(dir.join("config.toml").exists() && dir.join("config.yaml.migrated").exists() && !dir.join("config.yaml").exists());
        assert!(super::migrate_yaml_in(&dir).unwrap().is_none(), "second run converts nothing");
        std::fs::remove_file(dir.join("config.toml")).unwrap();
        std::fs::write(dir.join("config.yaml"), "terminal: [unclosed\n").unwrap();
        assert!(super::migrate_yaml_in(&dir).is_err());
        assert!(!dir.join("config.toml").exists(), "a broken YAML must not be hidden behind a default TOML");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dotted_probe_names_stay_one_key() {
        let c: super::Config =
            serde_yaml::from_str("network:\n  probes:\n    x.ai: https://example.com/\n    \"odd \\\"name\\\"\": https://example.org/\n").unwrap();
        let back: super::Config = toml::from_str(&super::config_text(&c)).unwrap();
        assert!(super::same(&c, &back));
        assert_eq!(back.network.probes.get("x.ai").map(String::as_str), Some("https://example.com/"));
    }

    #[test]
    fn theme_default_edit_keeps_every_other_line() {
        let dir = std::env::temp_dir().join(format!("adash-theme-default-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        let mut c = super::Config::default();
        c.terminal.emulator = "ghostty".into();
        std::fs::write(&p, super::config_text(&c) + "# my own note\n").unwrap();
        super::set_theme_default_in(&p, true, "ghostty-dracula").unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        let back: super::Config = toml::from_str(&text).unwrap();
        assert_eq!((back.theme.dark(), back.theme.light()), ("ghostty-dracula", "catppuccin-latte"));
        assert_eq!(back.terminal.emulator, "ghostty");
        assert!(text.contains("# my own note") && text.matches("dark = ").count() == 1, "{text}");
        // a file without a [theme] table gets one appended
        std::fs::write(&p, "port = 7900\n").unwrap();
        super::set_theme_default_in(&p, false, "nord").unwrap();
        let back: super::Config = toml::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!((back.port, back.theme.light()), (7900, "nord"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
