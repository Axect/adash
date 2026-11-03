//! `adash theme import <source>`: read a terminal or editor color scheme, normalize it to a
//! [`Palette`], derive the theme tokens with [`crate::palette::derive`] (the same mapping as the
//! built-in themes) and write a commented theme file. Every source is read-only.
//!
//! | source | auto-detected | otherwise |
//! |---|---|---|
//! | ghostty | config (+ `config-file` includes), `theme = name` or `light:…,dark:…` | config or theme file path |
//! | base16 / base24 | — | scheme YAML path, or a name looked up in `$BASE16_SCHEMES` / `~/.local/share/tinted-theming/schemes` |
//! | vscode | `workbench.colorTheme` from settings.json, theme JSON from installed or built-in extensions | theme JSON path |
//! | kitty | kitty.conf (+ `include` / `globinclude`) | conf path |
//! | alacritty | alacritty.toml (+ `import` / `general.import`) | TOML path |
//! | wezterm | `config.color_scheme = "Name"` in the Lua config, resolved in `~/.config/wezterm/colors` | scheme TOML path |
//! | iterm2 | — | `.itermcolors` path |
use crate::palette::{derive, Palette};
use crate::util::Res;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const SOURCES: &[&str] = &["ghostty", "base16", "base24", "vscode", "kitty", "alacritty", "wezterm", "iterm2"];

/// A scheme read from a source, before slotting.
#[derive(Debug, Default, Clone)]
pub struct Scheme {
    pub name: String,
    /// where each value came from (file and key), for the report
    pub origin: String,
    pub dark_hint: Option<bool>,
    pub bg: Option<String>,
    pub fg: Option<String>,
    pub selection: Option<String>,
    pub cursor: Option<String>,
    /// ANSI 0–15 (None where the source does not set it)
    pub ansi: [Option<String>; 16],
    /// colors with roles fixed by the source: (slot, hex, official name) — base16's base00…base0E
    pub named: Vec<(String, String, String)>,
    /// other published colors without a role: (name, hex)
    pub extra_named: Vec<(String, String)>,
    pub notes: Vec<String>,
}

fn home() -> PathBuf {
    crate::util::home()
}

fn expand(p: &str, rel_to: &Path) -> PathBuf {
    let p = p.trim().trim_matches('"');
    if let Some(r) = p.strip_prefix("~/") {
        home().join(r)
    } else if Path::new(p).is_absolute() {
        PathBuf::from(p)
    } else {
        rel_to.join(p)
    }
}

fn xdg_config() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home().join(".config"))
}

/// `#rgb`, `#rrggbb`, `rrggbb`, `0xrrggbb`, `#rrggbbaa` (alpha composited over `under` when given) → `#rrggbb`.
pub fn norm_hex(v: &str, under: Option<&str>) -> Option<String> {
    let v = v.trim().trim_matches(['"', '\'']);
    let h = v.strip_prefix('#').or_else(|| v.strip_prefix("0x")).or_else(|| v.strip_prefix("0X")).unwrap_or(v);
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let h = match h.len() {
        3 => h.chars().flat_map(|c| [c, c]).collect(),
        6 | 8 => h.to_string(),
        _ => return None,
    }
    .to_ascii_lowercase();
    if h.len() == 8 {
        let a = u8::from_str_radix(&h[6..8], 16).ok()? as f64 / 255.0;
        let u = norm_hex(under.unwrap_or("#000000"), None)?;
        let ch = |s: &str, i: usize| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or(0) as f64;
        let mix = |i: usize| ((ch(&h, i) * a + ch(&u[1..], i) * (1.0 - a)).round()) as u8;
        return Some(format!("#{:02x}{:02x}{:02x}", mix(0), mix(2), mix(4)));
    }
    Some(format!("#{h}"))
}

// ---------------------------------------------------------------- normalization

const SLOT_HUES: &[(&str, f64, f64)] = &[
    ("red", 345.0, 15.0),
    ("orange", 15.0, 45.0),
    ("yellow", 45.0, 70.0),
    ("green", 70.0, 160.0),
    ("cyan", 160.0, 200.0),
    ("blue", 200.0, 250.0),
    ("purple", 250.0, 290.0),
    ("magenta", 290.0, 345.0),
];
const ANSI_NAMES: [&str; 16] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "bright black",
    "bright red",
    "bright green",
    "bright yellow",
    "bright blue",
    "bright magenta",
    "bright cyan",
    "bright white",
];

fn hue_sat(c: &str) -> (f64, f64) {
    let p = |i: usize| u8::from_str_radix(&c[i..i + 2], 16).unwrap_or(0) as f64 / 255.0;
    let (r, g, b) = (p(1), p(3), p(5));
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let d = mx - mn;
    if d == 0.0 {
        return (0.0, 0.0);
    }
    let l = (mx + mn) / 2.0;
    let s = if l > 0.5 { d / (2.0 - mx - mn) } else { d / (mx + mn) };
    let h = if mx == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if mx == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s)
}

fn slot_of(c: &str) -> Option<&'static str> {
    let (h, s) = hue_sat(c);
    if s < 0.2 {
        return None;
    }
    SLOT_HUES.iter().find(|(_, a, b)| if a > b { h >= *a || h < *b } else { h >= *a && h < *b }).map(|x| x.0)
}

fn lum(c: &str) -> f64 {
    let p = |i: usize| u8::from_str_radix(&c[i..i + 2], 16).unwrap_or(0) as f64 / 255.0;
    let lin = |v: f64| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * lin(p(1)) + 0.7152 * lin(p(3)) + 0.0722 * lin(p(5))
}

/// Slot a scheme: bg/fg/selection from the source; hue slots by each ANSI color's actual hue
/// (normal colors first, bright ones fill gaps), since terminals do not agree on what "blue" or
/// "magenta" holds; bright black → comment, white → secondary text.
pub fn to_palette(s: &Scheme, id: &str) -> Res<Palette> {
    let mut p = Palette { id: id.to_string(), family: "user".into(), label: s.name.clone(), source: s.origin.clone(), ..Default::default() };
    let mut put = |slot: &str, hex: &str, name: &str| {
        p.colors.entry(slot.to_string()).or_insert((hex.to_string(), name.to_string()));
    };
    for (slot, hex, name) in &s.named {
        put(slot, hex, name);
    }
    let named = |slot: &str| s.named.iter().find(|x| x.0 == slot).map(|x| x.1.clone());
    let bg = s.bg.clone().or_else(|| named("bg")).or_else(|| s.ansi[0].clone()).ok_or("the scheme sets no background")?;
    let fg = s.fg.clone().or_else(|| named("fg")).or_else(|| s.ansi[7].clone()).ok_or("the scheme sets no foreground")?;
    put("bg", &bg, "background");
    put("fg", &fg, "foreground");
    if let Some(sel) = &s.selection {
        put("line", sel, "selection background");
    }
    if let Some(c) = &s.ansi[8] {
        put("comment", c, "bright black (color 8)");
    }
    if let Some(c) = &s.ansi[7] {
        if c != &fg {
            put("fg_dim", c, "white (color 7)");
        }
    }
    for pass in [1..7, 9..15] {
        for i in pass {
            if let Some(c) = &s.ansi[i] {
                if let Some(slot) = slot_of(c) {
                    put(slot, c, &format!("{} (color {i})", ANSI_NAMES[i]));
                }
            }
        }
    }
    let mut p2 = std::mem::take(&mut p);
    for (i, c) in s.ansi.iter().enumerate() {
        if let Some(c) = c {
            p2.extra.insert(format!("{} (color {i})", ANSI_NAMES[i]), Value::String(c.clone()));
        }
    }
    for (name, hex) in &s.extra_named {
        p2.extra.insert(name.clone(), Value::String(hex.clone()));
    }
    if let Some(c) = &s.cursor {
        p2.extra.insert("cursor".into(), Value::String(c.clone()));
    }
    p2.dark = s.dark_hint.unwrap_or_else(|| lum(&bg) < 0.2);
    Ok(p2)
}

// ---------------------------------------------------------------- key = value sources

/// How a format applies includes.
#[derive(Clone, Copy, PartialEq)]
enum Includes {
    /// kitty: the included file's lines take the place of the `include` line
    Inline,
    /// Ghostty: the whole current file first, then each `config-file` in order
    AfterFile,
}

/// `key = value` / `key value` lines with includes; later lines win (ghostty, kitty).
fn read_kv(
    path: &Path,
    sep: char,
    include_keys: &[&str],
    mode: Includes,
    depth: usize,
    out: &mut Vec<(String, String, PathBuf)>,
    notes: &mut Vec<String>,
) -> Res<()> {
    if depth > 8 {
        return Err(format!("{}: includes nested deeper than 8", path.display()));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut deferred: Vec<(PathBuf, bool)> = vec![];
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (k, v) = if sep == '=' {
            match line.split_once('=') {
                Some((k, v)) => (k.trim(), unquote(v.trim())),
                None => continue,
            }
        } else {
            match line.split_once(char::is_whitespace) {
                Some((k, v)) => (k.trim(), v.trim()),
                None => continue,
            }
        };
        if include_keys.contains(&k) {
            let (optional, v) = match v.strip_prefix('?') {
                Some(r) => (true, unquote(r)),
                None => (false, v),
            };
            let targets: Vec<(PathBuf, bool)> =
                if k == "globinclude" { glob(&expand(v, dir))?.into_iter().map(|p| (p, true)).collect() } else { vec![(expand(v, dir), optional)] };
            match mode {
                Includes::AfterFile => deferred.extend(targets),
                Includes::Inline => {
                    for (t, opt) in targets {
                        include_one(&t, opt, sep, include_keys, mode, depth, out, notes)?;
                    }
                }
            }
            continue;
        }
        out.push((k.to_string(), v.to_string(), path.to_path_buf()));
    }
    for (t, opt) in deferred {
        include_one(&t, opt, sep, include_keys, mode, depth, out, notes)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn include_one(
    t: &Path,
    optional: bool,
    sep: char,
    keys: &[&str],
    mode: Includes,
    depth: usize,
    out: &mut Vec<(String, String, PathBuf)>,
    notes: &mut Vec<String>,
) -> Res<()> {
    if t.exists() {
        read_kv(t, sep, keys, mode, depth + 1, out, notes)
    } else if optional {
        Ok(())
    } else {
        notes.push(format!("include {} not found (skipped)", t.display()));
        Ok(())
    }
}

/// A value in double quotes (Ghostty allows them) without the quotes.
fn unquote(v: &str) -> &str {
    v.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(v)
}

/// Shell-style glob with `*` and `?` in any path component (kitty `globinclude`). Other glob
/// syntax (`**`, `[…]`, `{…}`) is refused rather than silently matching nothing.
fn glob(p: &Path) -> Res<Vec<PathBuf>> {
    let s = p.to_string_lossy();
    if s.contains("**") || s.contains(['[', ']', '{', '}']) {
        return Err(format!("glob {s:?}: only * and ? are supported"));
    }
    let mut cur = vec![PathBuf::from(if s.starts_with('/') { "/" } else { "." })];
    for comp in p.components().filter(|c| !matches!(c, std::path::Component::RootDir)) {
        let c = comp.as_os_str().to_string_lossy().into_owned();
        let mut next = vec![];
        for base in &cur {
            if c.contains(['*', '?']) {
                let Ok(rd) = std::fs::read_dir(base) else { continue };
                let mut m: Vec<PathBuf> = rd.flatten().filter(|e| wildcard(&c, &e.file_name().to_string_lossy())).map(|e| e.path()).collect();
                m.sort();
                next.extend(m);
            } else {
                next.push(base.join(&c));
            }
        }
        cur = next;
    }
    Ok(cur.into_iter().filter(|p| p.is_file()).collect())
}

fn wildcard(pat: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], n) || (!n.is_empty() && go(p, &n[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &n[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &n[1..]),
            _ => false,
        }
    }
    let (p, n): (Vec<char>, Vec<char>) = (pat.chars().collect(), name.chars().collect());
    !name.starts_with('.') && go(&p, &n)
}

fn first_existing(cands: Vec<PathBuf>) -> Option<PathBuf> {
    cands.into_iter().find(|p| p.is_file())
}

// ghostty -------------------------------------------------------------------------------------

fn ghostty_config_paths() -> Vec<PathBuf> {
    let mut v = vec![xdg_config().join("ghostty/config"), xdg_config().join("ghostty/config.ghostty")];
    v.push(home().join("Library/Application Support/com.mitchellh.ghostty/config"));
    v.push(home().join("Library/Application Support/com.mitchellh.ghostty/config.ghostty"));
    v
}

fn ghostty_theme_dirs() -> Vec<PathBuf> {
    let mut v = vec![xdg_config().join("ghostty/themes")];
    if let Some(r) = std::env::var_os("GHOSTTY_RESOURCES_DIR") {
        v.push(PathBuf::from(r).join("themes"));
    }
    v.extend(
        ["/usr/share/ghostty/themes", "/usr/local/share/ghostty/themes", "/Applications/Ghostty.app/Contents/Resources/ghostty/themes"].map(PathBuf::from),
    );
    v.push(home().join(".local/share/ghostty/themes"));
    v
}

fn ghostty_apply(kv: &[(String, String, PathBuf)], s: &mut Scheme) {
    for (k, v, _) in kv {
        let bg = s.bg.clone();
        match k.as_str() {
            "background" => s.bg = norm_hex(v, None),
            "foreground" => s.fg = norm_hex(v, None),
            "selection-background" => s.selection = norm_hex(v, bg.as_deref()),
            "cursor-color" => s.cursor = norm_hex(v, None),
            "palette" => {
                if let Some((i, c)) = v.split_once('=') {
                    if let (Ok(i), Some(c)) = (i.trim().parse::<usize>(), norm_hex(c, None)) {
                        if i < 16 {
                            s.ansi[i] = Some(c);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn ghostty(arg: Option<&str>, want_dark: bool) -> Res<Scheme> {
    let cfg = match arg {
        Some(a) => PathBuf::from(a),
        None => first_existing(ghostty_config_paths()).ok_or("no Ghostty config found (~/.config/ghostty/config); pass a file")?,
    };
    let mut kv = vec![];
    let mut s = Scheme { name: "Ghostty".into(), origin: cfg.display().to_string(), ..Default::default() };
    read_kv(&cfg, '=', &["config-file"], Includes::AfterFile, 0, &mut kv, &mut s.notes)?;
    // theme first, then the config's own colors on top (Ghostty's precedence)
    if let Some((_, theme, _)) = kv.iter().rev().find(|(k, _, _)| k == "theme") {
        let chosen = if theme.contains(':') {
            let pick = theme.split(',').map(str::trim).find_map(|part| {
                let (mode, name) = part.split_once(':')?;
                ((mode.trim() == "dark") == want_dark).then(|| name.trim().to_string())
            });
            s.notes.push(format!("theme pair {theme:?}: imported the {} one", if want_dark { "dark" } else { "light" }));
            pick.ok_or_else(|| format!("theme {theme:?} has no {} entry", if want_dark { "dark" } else { "light" }))?
        } else {
            theme.clone()
        };
        let file = if Path::new(&chosen).is_absolute() {
            Some(PathBuf::from(&chosen))
        } else {
            // the config's own themes/ directory first (the user's ~/.config/ghostty/themes), then Ghostty's bundled ones
            cfg.parent().map(|d| d.join("themes")).into_iter().chain(ghostty_theme_dirs()).map(|d| d.join(&chosen)).find(|p| p.is_file())
        };
        let file = file.ok_or_else(|| {
            format!("Ghostty theme {chosen:?} not found in {}", ghostty_theme_dirs().iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(", "))
        })?;
        let mut tkv = vec![];
        read_kv(&file, '=', &[], Includes::AfterFile, 0, &mut tkv, &mut s.notes)?;
        ghostty_apply(&tkv, &mut s);
        s.name = chosen.clone();
        s.origin = format!("{} (theme {chosen:?} from {})", cfg.display(), file.display());
    }
    ghostty_apply(&kv, &mut s);
    Ok(s)
}

// kitty ---------------------------------------------------------------------------------------

fn kitty(arg: Option<&str>) -> Res<Scheme> {
    let cfg = match arg {
        Some(a) => PathBuf::from(a),
        None => {
            let mut c = vec![];
            if let Some(d) = std::env::var_os("KITTY_CONFIG_DIRECTORY") {
                c.push(PathBuf::from(d).join("kitty.conf"));
            }
            c.push(xdg_config().join("kitty/kitty.conf"));
            c.push(home().join("Library/Preferences/kitty/kitty.conf"));
            first_existing(c).ok_or("no kitty.conf found; pass a file")?
        }
    };
    let mut kv = vec![];
    let mut s = Scheme { name: "kitty".into(), origin: cfg.display().to_string(), ..Default::default() };
    read_kv(&cfg, ' ', &["include", "globinclude"], Includes::Inline, 0, &mut kv, &mut s.notes)?;
    for (k, v, _) in &kv {
        let bg = s.bg.clone();
        match k.as_str() {
            "background" => s.bg = norm_hex(v, None),
            "foreground" => s.fg = norm_hex(v, None),
            "selection_background" => s.selection = norm_hex(v, bg.as_deref()),
            "cursor" => s.cursor = norm_hex(v, None),
            k if k.starts_with("color") => {
                if let Ok(i) = k[5..].parse::<usize>() {
                    if i < 16 {
                        s.ansi[i] = norm_hex(v, None);
                    }
                }
            }
            _ => {}
        }
    }
    if let Some((_, _, f)) = kv.iter().find(|(k, _, _)| k == "background") {
        if f.file_name().is_some_and(|n| n == "current-theme.conf") {
            s.name = "kitty current theme".into();
        }
    }
    Ok(s)
}

// alacritty -----------------------------------------------------------------------------------

fn alacritty(arg: Option<&str>) -> Res<Scheme> {
    let cfg = match arg {
        Some(a) => PathBuf::from(a),
        None => first_existing(vec![xdg_config().join("alacritty/alacritty.toml"), xdg_config().join("alacritty.toml"), home().join(".alacritty.toml")])
            .ok_or_else(|| {
                if xdg_config().join("alacritty/alacritty.yml").exists() {
                    "only a legacy alacritty.yml found; convert it with `alacritty migrate` first".to_string()
                } else {
                    "no alacritty.toml found; pass a file".to_string()
                }
            })?,
    };
    if cfg.extension().is_some_and(|e| e == "yml" || e == "yaml") {
        return Err("legacy YAML config: convert it with `alacritty migrate`, then import the TOML".into());
    }
    let mut s = Scheme { name: "Alacritty".into(), origin: cfg.display().to_string(), ..Default::default() };
    alacritty_read(&cfg, &mut s, 0)?;
    Ok(s)
}

fn alacritty_read(path: &Path, s: &mut Scheme, depth: usize) -> Res<()> {
    if depth > 8 {
        return Err("imports nested deeper than 8".into());
    }
    let t: toml::Table =
        std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?.parse().map_err(|e| format!("{}: {e}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let imports = t.get("general").and_then(|g| g.get("import")).or_else(|| t.get("import")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    for i in imports.iter().filter_map(|v| v.as_str()) {
        let p = expand(i, dir);
        if p.exists() {
            alacritty_read(&p, s, depth + 1)?;
        } else {
            s.notes.push(format!("import {} not found (skipped)", p.display()));
        }
    }
    let Some(c) = t.get("colors") else { return Ok(()) };
    let get = |a: &str, b: &str| c.get(a).and_then(|x| x.get(b)).and_then(|v| v.as_str()).and_then(|v| norm_hex(v, None));
    if let Some(v) = get("primary", "background") {
        s.bg = Some(v);
    }
    if let Some(v) = get("primary", "foreground") {
        s.fg = Some(v);
    }
    if let Some(v) = get("selection", "background") {
        s.selection = Some(v);
    }
    if let Some(v) = get("cursor", "cursor") {
        s.cursor = Some(v);
    }
    for (off, group) in [(0, "normal"), (8, "bright")] {
        for (i, n) in ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"].iter().enumerate() {
            if let Some(v) = get(group, n) {
                s.ansi[off + i] = Some(v);
            }
        }
    }
    Ok(())
}

// wezterm -------------------------------------------------------------------------------------

fn wezterm(arg: Option<&str>) -> Res<Scheme> {
    let file = match arg.map(PathBuf::from).filter(|p| p.is_file()) {
        Some(p) => p,
        None => {
            let name = match arg {
                Some(n) => n.to_string(),
                None => {
                    let mut c = vec![];
                    if let Some(f) = std::env::var_os("WEZTERM_CONFIG_FILE") {
                        c.push(PathBuf::from(f));
                    }
                    c.push(xdg_config().join("wezterm/wezterm.lua"));
                    c.push(home().join(".wezterm.lua"));
                    let cfg = first_existing(c).ok_or("no wezterm.lua found; pass a scheme TOML file")?;
                    let text = std::fs::read_to_string(&cfg).map_err(|e| e.to_string())?;
                    let re = regex::Regex::new(r#"color_scheme\s*=\s*["']([^"']+)["']"#).unwrap();
                    re.captures(&text).map(|c| c[1].to_string()).ok_or_else(|| {
                        format!("{}: no plain `color_scheme = \"Name\"` assignment (Lua is not evaluated); pass a scheme TOML file", cfg.display())
                    })?
                }
            };
            let dir = xdg_config().join("wezterm/colors");
            let found = std::fs::read_dir(&dir).ok().and_then(|rd| {
                rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml")).find(|p| {
                    p.file_stem().is_some_and(|s| s.to_string_lossy() == name)
                        || std::fs::read_to_string(p)
                            .ok()
                            .and_then(|t| t.parse::<toml::Table>().ok())
                            .is_some_and(|t| t.get("metadata").and_then(|m| m.get("name")).and_then(|v| v.as_str()) == Some(name.as_str()))
                })
            });
            found.ok_or_else(|| format!("scheme {name:?} is not in {} (WezTerm's bundled schemes are inside the binary); pass its TOML file", dir.display()))?
        }
    };
    let t: toml::Table = std::fs::read_to_string(&file).map_err(|e| e.to_string())?.parse().map_err(|e| format!("{}: {e}", file.display()))?;
    let c = t.get("colors").ok_or_else(|| format!("{}: no [colors] table", file.display()))?;
    let g = |k: &str| c.get(k).and_then(|v| v.as_str()).and_then(|v| norm_hex(v, None));
    let mut s = Scheme {
        name: t
            .get("metadata")
            .and_then(|m| m.get("name"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| file.file_stem().unwrap_or_default().to_string_lossy().into_owned()),
        origin: file.display().to_string(),
        bg: g("background"),
        fg: g("foreground"),
        cursor: g("cursor_bg"),
        ..Default::default()
    };
    s.selection = c.get("selection_bg").and_then(|v| v.as_str()).and_then(|v| norm_hex(v, s.bg.as_deref()));
    for (off, k) in [(0, "ansi"), (8, "brights")] {
        for (i, v) in c.get(k).and_then(|v| v.as_array()).into_iter().flatten().enumerate().take(8) {
            s.ansi[off + i] = v.as_str().and_then(|v| norm_hex(v, None));
        }
    }
    Ok(s)
}

// iterm2 --------------------------------------------------------------------------------------

/// `.itermcolors` (XML plist): `<key>Ansi N Color</key><dict>… Red/Green/Blue Component …</dict>`.
fn iterm2(arg: Option<&str>) -> Res<Scheme> {
    let file = arg.map(PathBuf::from).ok_or("pass an .itermcolors file (iTerm2 → Settings → Profiles → Colors → Color Presets → Export)")?;
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    if !text.trim_start().starts_with("<?xml") && !text.contains("<plist") {
        return Err(format!("{}: not an XML plist (binary plists: convert with `plutil -convert xml1`)", file.display()));
    }
    let entry = regex::Regex::new(r"(?s)<key>([^<]+)</key>\s*<dict>(.*?)</dict>").unwrap();
    let comp = regex::Regex::new(r"(?s)<key>(Red|Green|Blue|Alpha) Component</key>\s*<real>([0-9.eE+-]+)</real>").unwrap();
    let space = regex::Regex::new(r"(?s)<key>Color Space</key>\s*<string>([^<]+)</string>").unwrap();
    let mut s = Scheme { name: file.file_stem().unwrap_or_default().to_string_lossy().into_owned(), origin: file.display().to_string(), ..Default::default() };
    let mut spaces = std::collections::BTreeSet::new();
    let mut colors: BTreeMap<String, String> = BTreeMap::new();
    for e in entry.captures_iter(&text) {
        let mut rgb = [0.0f64; 3];
        for c in comp.captures_iter(&e[2]) {
            let v: f64 = c[2].parse().unwrap_or(0.0);
            match &c[1] {
                "Red" => rgb[0] = v,
                "Green" => rgb[1] = v,
                "Blue" => rgb[2] = v,
                _ => {}
            }
        }
        if let Some(sp) = space.captures(&e[2]) {
            spaces.insert(sp[1].to_string());
        }
        let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        colors.insert(e[1].trim().to_string(), format!("#{:02x}{:02x}{:02x}", b(rgb[0]), b(rgb[1]), b(rgb[2])));
    }
    if colors.is_empty() {
        return Err(format!("{}: no colors found", file.display()));
    }
    for i in 0..16 {
        s.ansi[i] = colors.get(&format!("Ansi {i} Color")).cloned();
    }
    s.bg = colors.get("Background Color").cloned();
    s.fg = colors.get("Foreground Color").cloned();
    s.selection = colors.get("Selection Color").cloned();
    s.cursor = colors.get("Cursor Color").cloned();
    if !spaces.is_empty() {
        s.notes.push(format!("color space {} taken as sRGB components", spaces.into_iter().collect::<Vec<_>>().join(", ")));
    }
    Ok(s)
}

// base16 / base24 -----------------------------------------------------------------------------

/// base16 styling guide roles.
const BASE16_ROLES: &[(&str, &str)] = &[
    ("base00", "bg"),
    ("base01", "bg_alt"),
    ("base02", "line"),
    ("base03", "comment"),
    ("base04", "fg_dim"),
    ("base05", "fg"),
    ("base08", "red"),
    ("base09", "orange"),
    ("base0A", "yellow"),
    ("base0B", "green"),
    ("base0C", "cyan"),
    ("base0D", "blue"),
    ("base0E", "purple"),
];

fn base16(arg: Option<&str>, system: &str) -> Res<Scheme> {
    let a = arg.ok_or_else(|| format!("pass a {system} scheme YAML file or a scheme name"))?;
    let file = if Path::new(a).is_file() {
        PathBuf::from(a)
    } else {
        let mut dirs = vec![];
        if let Some(d) = std::env::var_os("BASE16_SCHEMES") {
            dirs.push(PathBuf::from(d));
        }
        dirs.push(home().join(".local/share/tinted-theming/schemes"));
        dirs.iter()
            .flat_map(|d| [d.join(system).join(format!("{a}.yaml")), d.join(format!("{a}.yaml"))])
            .find(|p| p.is_file())
            .ok_or_else(|| format!("scheme {a:?} not found in $BASE16_SCHEMES or ~/.local/share/tinted-theming/schemes; pass the YAML file"))?
    };
    let y: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(&file).map_err(|e| e.to_string())?).map_err(|e| format!("{}: {e}", file.display()))?;
    // tinted-theming (`palette:` map) or the classic flat form
    let pal = y.get("palette").unwrap_or(&y);
    let get = |k: &str| pal.get(k).and_then(|v| v.as_str()).and_then(|v| norm_hex(v, None));
    let name = y.get("name").or_else(|| y.get("scheme")).and_then(|v| v.as_str()).unwrap_or("base16").to_string();
    let mut s = Scheme { name, origin: file.display().to_string(), ..Default::default() };
    s.dark_hint = y.get("variant").and_then(|v| v.as_str()).map(|v| v != "light");
    for (k, slot) in BASE16_ROLES {
        if let Some(c) = get(k) {
            s.named.push((slot.to_string(), c, k.to_string()));
        }
    }
    // base24 adds darker backgrounds (base10, base11) and bright accents (base12–base17)
    let more: &[&str] = if system == "base24" {
        &["base06", "base07", "base0F", "base10", "base11", "base12", "base13", "base14", "base15", "base16", "base17"]
    } else {
        &["base06", "base07", "base0F"]
    };
    for k in more {
        if let Some(c) = get(k) {
            s.extra_named.push((k.to_string(), c));
        }
    }
    if get("base00").is_none() || get("base05").is_none() {
        return Err(format!("{}: base00 and base05 are required", file.display()));
    }
    Ok(s)
}

// vscode --------------------------------------------------------------------------------------

/// JSON with comments and trailing commas → JSON.
pub fn jsonc(text: &str) -> Res<Value> {
    let mut out = String::with_capacity(text.len());
    let mut it = text.chars().peekable();
    let mut in_str = false;
    while let Some(c) = it.next() {
        if in_str {
            out.push(c);
            if c == '\\' {
                if let Some(n) = it.next() {
                    out.push(n);
                }
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match (c, it.peek()) {
            ('"', _) => {
                in_str = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for n in it.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                it.next();
                let mut prev = ' ';
                for n in it.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    let trailing = regex::Regex::new(r",(\s*[}\]])").unwrap();
    serde_json::from_str(&trailing.replace_all(&out, "$1")).map_err(|e| format!("JSON: {e}"))
}

fn vscode_user_dirs() -> Vec<PathBuf> {
    let apps = ["Code", "VSCodium", "Cursor", "Code - OSS", "Code - Insiders"];
    let mut v: Vec<PathBuf> = apps.iter().map(|a| xdg_config().join(a).join("User/settings.json")).collect();
    v.extend(apps.iter().map(|a| home().join("Library/Application Support").join(a).join("User/settings.json")));
    v
}

fn vscode_extension_dirs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> =
        [".vscode/extensions", ".vscode-oss/extensions", ".cursor/extensions", ".vscode-insiders/extensions"].iter().map(|d| home().join(d)).collect();
    v.extend(
        [
            "/usr/share/code/resources/app/extensions",
            "/usr/lib/code/extensions",
            "/opt/visual-studio-code/resources/app/extensions",
            "/usr/share/vscodium/resources/app/extensions",
            "/opt/vscodium-bin/resources/app/extensions",
            "/Applications/Visual Studio Code.app/Contents/Resources/app/extensions",
            "/Applications/VSCodium.app/Contents/Resources/app/extensions",
            "/Applications/Cursor.app/Contents/Resources/app/extensions",
        ]
        .map(PathBuf::from),
    );
    v
}

fn vscode_find_theme(label: &str) -> Option<PathBuf> {
    for d in vscode_extension_dirs() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let pkg = e.path().join("package.json");
            let Ok(t) = std::fs::read_to_string(&pkg) else { continue };
            let Ok(v) = jsonc(&t) else { continue };
            for th in v["contributes"]["themes"].as_array().into_iter().flatten() {
                if th["label"].as_str() == Some(label) || th["id"].as_str() == Some(label) {
                    if let Some(p) = th["path"].as_str() {
                        return Some(e.path().join(p));
                    }
                }
            }
        }
    }
    None
}

fn vscode(arg: Option<&str>) -> Res<Scheme> {
    let file = match arg {
        Some(a) if Path::new(a).is_file() => PathBuf::from(a),
        Some(label) => {
            vscode_find_theme(label).ok_or_else(|| format!("VS Code theme {label:?} not found in installed or built-in extensions; pass the theme JSON"))?
        }
        None => {
            let settings = first_existing(vscode_user_dirs()).ok_or("no VS Code settings.json found; pass a theme JSON file")?;
            let v = jsonc(&std::fs::read_to_string(&settings).map_err(|e| e.to_string())?).map_err(|e| format!("{}: {e}", settings.display()))?;
            let label = v["workbench.colorTheme"]
                .as_str()
                .ok_or_else(|| format!("{}: no workbench.colorTheme (the default theme is not shipped as a file)", settings.display()))?;
            vscode_find_theme(label).ok_or_else(|| {
                format!("VS Code theme {label:?} (from {}) not found in installed or built-in extensions; pass the theme JSON", settings.display())
            })?
        }
    };
    let mut colors: BTreeMap<String, String> = BTreeMap::new();
    let mut tokens: Vec<Value> = vec![];
    let mut name = None;
    vscode_read(&file, &mut colors, &mut tokens, &mut name, 0)?;
    let mut s = Scheme {
        name: name.unwrap_or_else(|| file.file_stem().unwrap_or_default().to_string_lossy().into_owned()),
        origin: file.display().to_string(),
        ..Default::default()
    };
    s.bg = colors.get("editor.background").and_then(|v| norm_hex(v, None));
    let bg = s.bg.clone();
    s.fg = colors.get("editor.foreground").or_else(|| colors.get("foreground")).and_then(|v| norm_hex(v, bg.as_deref()));
    s.selection = colors.get("editor.selectionBackground").and_then(|v| norm_hex(v, bg.as_deref()));
    s.cursor = colors.get("editorCursor.foreground").and_then(|v| norm_hex(v, None));
    let ansi = ["Black", "Red", "Green", "Yellow", "Blue", "Magenta", "Cyan", "White"];
    for (i, n) in ansi.iter().enumerate() {
        s.ansi[i] = colors.get(&format!("terminal.ansi{n}")).and_then(|v| norm_hex(v, bg.as_deref()));
        s.ansi[8 + i] = colors.get(&format!("terminal.ansiBright{n}")).and_then(|v| norm_hex(v, bg.as_deref()));
    }
    if s.ansi.iter().filter(|c| c.is_some()).count() < 6 {
        // no terminal colors: take hues from the syntax token colors
        s.notes.push("the theme has no terminal.ansi* colors; hues taken from its syntax token colors".into());
        let scope_color = |want: &[&str]| {
            tokens.iter().find_map(|t| {
                let scopes: Vec<String> = match &t["scope"] {
                    Value::String(s) => s.split(',').map(|x| x.trim().to_string()).collect(),
                    Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
                    _ => vec![],
                };
                scopes
                    .iter()
                    .any(|sc| want.iter().any(|w| sc == w || sc.starts_with(&format!("{w}."))))
                    .then(|| t["settings"]["foreground"].as_str().and_then(|v| norm_hex(v, bg.as_deref())))
                    .flatten()
            })
        };
        for (want, slot, nm) in [
            (&["comment"][..], "comment", "comment"),
            (&["invalid", "markup.deleted"][..], "red", "invalid"),
            (&["constant.numeric", "constant"][..], "orange", "constant"),
            (&["entity.name.type", "support.type", "storage.type"][..], "yellow", "type"),
            (&["string"][..], "green", "string"),
            (&["support.function", "string.regexp"][..], "cyan", "support"),
            (&["entity.name.function"][..], "blue", "function"),
            (&["keyword", "storage"][..], "purple", "keyword"),
        ] {
            if let Some(c) = scope_color(want) {
                s.named.push((slot.to_string(), c, format!("token color: {nm}")));
            }
        }
    }
    Ok(s)
}

fn vscode_read(path: &Path, colors: &mut BTreeMap<String, String>, tokens: &mut Vec<Value>, name: &mut Option<String>, depth: usize) -> Res<()> {
    if depth > 8 {
        return Err("theme includes nested deeper than 8".into());
    }
    let v = jsonc(&std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some(inc) = v["include"].as_str() {
        vscode_read(&path.parent().unwrap_or(Path::new(".")).join(inc), colors, tokens, name, depth + 1)?;
    }
    if let Some(n) = v["name"].as_str() {
        *name = Some(n.to_string());
    }
    for (k, c) in v["colors"].as_object().into_iter().flatten() {
        if let Some(c) = c.as_str() {
            colors.insert(k.clone(), c.to_string());
        }
    }
    let mut own: Vec<Value> = v["tokenColors"].as_array().cloned().unwrap_or_default();
    own.extend(std::mem::take(tokens));
    *tokens = own;
    Ok(())
}

// ---------------------------------------------------------------- pipeline

pub struct Imported {
    pub palette: Palette,
    pub text: String,
    pub report: Vec<String>,
}

/// Read `source` (auto-discovered when `arg` is None), derive and check. `want_dark` picks the
/// dark half of a light/dark pair.
pub fn import(source: &str, arg: Option<&str>, name: Option<&str>, want_dark: bool) -> Res<Imported> {
    let s = match source {
        "ghostty" => ghostty(arg, want_dark)?,
        "kitty" => kitty(arg)?,
        "alacritty" => alacritty(arg)?,
        "wezterm" => wezterm(arg)?,
        "iterm2" => iterm2(arg)?,
        "base16" | "base24" => base16(arg, source)?,
        "vscode" => vscode(arg)?,
        o => return Err(format!("unknown source {o}; one of {}", SOURCES.join(", "))),
    };
    let slug: String = s
        .name
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let id = name.map(String::from).unwrap_or_else(|| if slug.is_empty() || slug == source { source.to_string() } else { format!("{source}-{slug}") });
    if !crate::theme::valid_name(&id) {
        return Err(format!("theme name {id:?}: use letters, digits, - and _ (--name)"));
    }
    let mut palette = to_palette(&s, &id)?;
    palette.label = format!("{} ({source})", s.name);
    let derived = derive(&palette)?;
    let mut report = s.notes.clone();
    // where the mapping had to move away from the obvious slot
    let (e, a, t) = &derived.needs_me;
    report
        .push(format!("error = {}, question = {}, my turn = {}", derived.source["signal.error"], derived.source["signal.ask"], derived.source["signal.turn"]));
    for (role, key, want) in [("error", e, "red"), ("question", a, "purple")] {
        if key != want {
            report.push(format!(
                "{role}: used {} ({want} {})",
                derived.source[if role == "error" { "signal.error" } else { "signal.ask" }],
                if palette.colors.contains_key(want) { "misses 3:1 or is taken" } else { "missing" }
            ));
        }
    }
    if t != "orange" {
        report.push(format!("my turn: used {} (orange missing, too faint or taken)", derived.source["signal.turn"]));
    }
    for k in ["signal.ask-text", "signal.error-text", "signal.turn-text", "ui.link", "info.warn", "info.thread"] {
        if derived.source[k] == palette.colors.get("fg").map(|x| x.1.clone()).unwrap_or_default() {
            report.push(format!("{k}: foreground (no palette color of that hue reads at 4.5:1)"));
        }
    }
    let text = crate::palette::to_toml(&palette, &derived, &format!(", imported from {source}"));
    Ok(Imported { palette, text, report })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        format!("{}/src/testdata/import/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    /// Every source reads its fixture, slots the same synthetic scheme and passes the checker.
    #[test]
    fn every_source_imports_its_fixture_and_passes_the_checker() {
        for (src, file) in [
            ("ghostty", "ghostty/config"),
            ("kitty", "kitty/kitty.conf"),
            ("alacritty", "alacritty/alacritty.toml"),
            ("wezterm", "wezterm/Sample.toml"),
            ("iterm2", "sample.itermcolors"),
            ("base16", "base16-sample.yaml"),
            ("base24", "base24-sample.yaml"),
            ("vscode", "vscode/sample-color-theme.json"),
        ] {
            let i = import(src, Some(&fixture(file)), Some(&format!("t-{src}")), true).unwrap_or_else(|e| panic!("{src}: {e}"));
            assert_eq!(i.palette.colors["bg"].0, "#1e2030", "{src}: background");
            assert_eq!(i.palette.colors["fg"].0, "#d8dae6", "{src}: foreground");
            let t: toml::Table = i.text.parse().unwrap();
            assert_eq!(t["signal"]["error"].as_str(), Some("#e06c75"), "{src}: error from the red");
            let (w, e) = crate::theme::check_text(&i.palette.id, &i.text);
            assert!(w.is_empty() && e.is_empty(), "{src}: {w:?} {e:?}\n{}", i.text);
        }
    }

    #[test]
    fn ghostty_follows_includes_and_theme_pairs() {
        let i = import("ghostty", Some(&fixture("ghostty/pair.config")), Some("t-pair"), true).unwrap();
        assert_eq!(i.palette.colors["bg"].0, "#1e2030", "dark half of light:…,dark:…");
        assert!(i.report.iter().any(|r| r.contains("imported the dark one")));
        let l = import("ghostty", Some(&fixture("ghostty/pair.config")), Some("t-pair-l"), false).unwrap();
        assert_eq!(l.palette.colors["bg"].0, "#f6f6f2", "light half");
        let inc = import("ghostty", Some(&fixture("ghostty/include.config")), Some("t-inc"), true).unwrap();
        assert_eq!(inc.palette.colors["fg"].0, "#d8dae6", "colors from config-file include");
        let missing = import("ghostty", Some(&fixture("ghostty/missing-theme.config")), None, true);
        assert!(missing.is_err_and(|e| e.contains("not found")));
    }

    #[test]
    fn hex_forms_and_alpha() {
        assert_eq!(norm_hex("0xAABBCC", None).as_deref(), Some("#aabbcc"));
        assert_eq!(norm_hex("abc", None).as_deref(), Some("#aabbcc"));
        assert_eq!(norm_hex("#ffffff80", Some("#000000")).as_deref(), Some("#808080"));
        assert!(norm_hex("#12", None).is_none());
        assert_eq!(jsonc("{\"a\": 1, // c\n /* x */ \"b\": [1,2,],}").unwrap(), serde_json::json!({"a": 1, "b": [1, 2]}));
    }

    /// The user's own Ghostty config (read-only), when present on this machine.
    #[test]
    fn real_ghostty_config_imports_cleanly() {
        let Some(cfg) = ghostty_config_paths().into_iter().find(|p| p.is_file()) else { return };
        let i = import("ghostty", Some(cfg.to_str().unwrap()), Some("t-real"), true).unwrap();
        let (w, e) = crate::theme::check_text(&i.palette.id, &i.text);
        assert!(w.is_empty() && e.is_empty(), "{w:?} {e:?}");
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("adash-import-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn ghostty_applies_config_file_after_the_whole_file_and_reads_quoted_values() {
        let d = tmp("gh");
        std::fs::write(d.join("colors.inc"), std::fs::read_to_string(fixture("ghostty/colors.inc")).unwrap()).unwrap();
        // the include comes first in the file but is applied after it: the include's background wins
        std::fs::write(d.join("config"), "config-file = colors.inc\nbackground = #000000\n").unwrap();
        let i = import("ghostty", Some(d.join("config").to_str().unwrap()), Some("t-order"), true).unwrap();
        assert_eq!(i.palette.colors["bg"].0, "#1e2030");
        let theme = fixture("ghostty/themes/Sample Night");
        std::fs::write(d.join("quoted"), format!("theme = \"{theme}\"\n")).unwrap();
        let q = import("ghostty", Some(d.join("quoted").to_str().unwrap()), Some("t-quoted"), true).unwrap();
        assert_eq!(q.palette.colors["fg"].0, "#d8dae6");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn kitty_globinclude_expands_directory_wildcards_and_refuses_other_syntax() {
        let d = tmp("kitty");
        std::fs::create_dir_all(d.join("themes/night")).unwrap();
        std::fs::write(d.join("themes/night/colors.conf"), std::fs::read_to_string(fixture("kitty/colors.conf")).unwrap()).unwrap();
        std::fs::write(d.join("kitty.conf"), "globinclude themes/*/colors.conf\n").unwrap();
        let i = import("kitty", Some(d.join("kitty.conf").to_str().unwrap()), Some("t-glob"), true).unwrap();
        assert_eq!(i.palette.colors["bg"].0, "#1e2030");
        std::fs::write(d.join("bad.conf"), "globinclude themes/**/colors.conf\n").unwrap();
        assert!(import("kitty", Some(d.join("bad.conf").to_str().unwrap()), None, true).is_err_and(|e| e.contains("only * and ?")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn names_with_quotes_still_produce_valid_toml() {
        let d = tmp("b16");
        let y = std::fs::read_to_string(fixture("base16-sample.yaml")).unwrap().replace("name: \"Sample Night\"", "name: 'A \"B\nC'");
        std::fs::write(d.join("s.yaml"), y).unwrap();
        let i = import("base16", Some(d.join("s.yaml").to_str().unwrap()), Some("t-quote"), true).unwrap();
        let t: toml::Table = i.text.parse().expect("valid TOML");
        assert_eq!(t["label"].as_str(), Some("A \"B C (base16)"), "YAML folds the line break; the quote survives");
        let _ = std::fs::remove_dir_all(&d);
    }
}
