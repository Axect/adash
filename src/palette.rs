//! Color palettes → adash themes. A palette is a published color scheme (Dracula, Catppuccin, a
//! terminal's 16 colors, …) with each color under a slot (`bg`, `fg`, `red`, …) and its official
//! name. [`derive`] maps the palette to the theme tokens under the checker's rules, so built-in
//! themes and imported terminal themes come from one implementation:
//!
//! - bars, chips and marks use palette colors (3:1 on the card surface);
//! - text reaches 4.5:1: the slot's own color if it does, else the palette color nearest in hue
//!   that does, else the foreground;
//! - bold text on solid fills (buttons, answered question numbers) reaches 3:1;
//! - needs-me order error > question > my turn: my turn is a calm hue (orange, cyan, green), never
//!   the brightest yellow of a dark palette, and differs from the interaction, question and error
//!   colors;
//! - translucent chip and selection backgrounds are palette colors at reduced alpha; no new colors.
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Palette {
    pub id: String,
    pub family: String,
    pub label: String,
    pub dark: bool,
    pub source: String,
    /// color-blind-safe palette: error = red slot, question = blue (or magenta)
    #[serde(default)]
    pub cvd: bool,
    /// slot → [hex, official name]
    pub colors: BTreeMap<String, (String, String)>,
    /// every other published color: name → hex
    #[serde(default)]
    pub extra: BTreeMap<String, serde_json::Value>,
    /// role overrides for palettes whose accents miss the contrast: interact / ask / error / turn → key
    #[serde(default)]
    pub roles: BTreeMap<String, String>,
    /// preferred my-turn keys, tried first
    #[serde(default)]
    pub turn: Vec<String>,
}

/// Derived tokens with the palette color behind each value.
#[derive(Clone, Debug, Default)]
pub struct Derived {
    pub tokens: BTreeMap<String, String>,
    pub source: BTreeMap<String, String>,
    /// palette keys of (error, question, my turn)
    pub needs_me: (String, String, String),
}

fn rgb(c: &str) -> (f64, f64, f64) {
    let h = c.trim_start_matches('#');
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0) as f64 / 255.0;
    (p(0), p(2), p(4))
}

fn over(c: &str, alpha: u8, under: &str) -> String {
    let (a, u) = (rgb(c), rgb(under));
    let t = alpha as f64 / 255.0;
    let m = |x: f64, y: f64| ((x * t + y * (1.0 - t)) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", m(a.0, u.0), m(a.1, u.1), m(a.2, u.2))
}

fn lum(c: &str) -> f64 {
    let (r, g, b) = rgb(c);
    let lin = |v: f64| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

fn cr(a: &str, b: &str) -> f64 {
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// (hue in degrees, HSL saturation)
fn hs(c: &str) -> (f64, f64) {
    let (r, g, b) = rgb(c);
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let l = (mx + mn) / 2.0;
    let d = mx - mn;
    if d == 0.0 {
        return (0.0, 0.0);
    }
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

fn hue_dist(a: &str, b: &str) -> f64 {
    let d = (hs(a).0 - hs(b).0).abs() % 360.0;
    d.min(360.0 - d)
}

const CHIP_ALPHAS: &[u8] = &[0x1f, 0x1a, 0x14, 0x0f, 0x0a, 0x08, 0x06, 0x04, 0x00];
const SELECTION_ALPHAS: &[u8] = &[0x26, 0x1f, 0x1a, 0x14, 0x0f, 0x0a, 0x06, 0x04, 0x02, 0x00];
const HUES: &[&str] = &["red", "orange", "yellow", "green", "cyan", "blue", "purple", "magenta"];

fn pct(a: u8) -> u32 {
    a as u32 * 100 / 255
}

fn is_hex6(v: &str) -> bool {
    v.len() == 7 && v.starts_with('#') && v[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Map a palette to theme tokens. Errors name what the palette lacks.
pub fn derive(p: &Palette) -> Result<Derived, String> {
    // key → (hex, name): slots first, then extras as `x:<name>`
    let mut all: Vec<(String, String, String)> = vec![];
    for (k, (hex, name)) in &p.colors {
        if !is_hex6(hex) {
            return Err(format!("{}: color {k} = {hex:?} is not #rrggbb", p.id));
        }
        all.push((k.clone(), hex.to_ascii_lowercase(), name.clone()));
    }
    for (name, v) in &p.extra {
        if let Some(hex) = v.as_str().filter(|h| is_hex6(h)) {
            all.push((format!("x:{name}"), hex.to_ascii_lowercase(), name.clone()));
        }
    }
    let get = |k: &str| all.iter().find(|x| x.0 == k).map(|x| x.1.clone());
    let name = |k: &str| all.iter().find(|x| x.0 == k).map(|x| x.2.clone()).unwrap_or_else(|| k.to_string());
    let slot = |k: &str| p.colors.get(k).map(|x| x.0.to_ascii_lowercase());
    let surface = slot("bg").ok_or("palette has no bg")?;
    let fg = slot("fg").ok_or("palette has no fg")?;
    if cr(&fg, &surface) < 4.5 {
        return Err(format!("{}: fg on bg is {:.2}:1, under 4.5:1", p.id, cr(&fg, &surface)));
    }
    let mut d = Derived::default();
    let set = |d: &mut Derived, token: &str, value: String, src: String| {
        d.tokens.insert(token.to_string(), value);
        d.source.insert(token.to_string(), src);
    };
    let pick = |d: &mut Derived, token: &str, key: &str| {
        let v = if key == "fg" { fg.clone() } else { get(key).unwrap_or_else(|| fg.clone()) };
        let n = if key == "fg" { name("fg") } else { name(key) };
        d.tokens.insert(token.to_string(), v);
        d.source.insert(token.to_string(), n);
    };

    pick(&mut d, "ui.surface", "bg");
    pick(&mut d, "ui.fg", "fg");
    let readable_bg = |k: &'static str| slot(k).filter(|c| cr(&fg, c) >= 4.5).map(|_| k).unwrap_or("bg");
    pick(&mut d, "ui.bg", readable_bg("bg_dark"));
    pick(&mut d, "ui.subtle", readable_bg("bg_alt"));
    match ["fg_dim", "comment"].into_iter().find(|k| slot(k).is_some_and(|c| cr(&c, &surface) >= 4.5)) {
        Some(k) => pick(&mut d, "ui.muted", k),
        None => {
            let a = [0xc7u8, 0xd9, 0xe6, 0xf2].into_iter().find(|a| cr(&over(&fg, *a, &surface), &surface) >= 4.5);
            match a {
                Some(a) => set(&mut d, "ui.muted", format!("{fg}{a:02x}"), format!("{} at {}%", name("fg"), pct(a))),
                None => pick(&mut d, "ui.muted", "fg"),
            }
        }
    }
    let muted = (d.tokens["ui.muted"].clone(), d.source["ui.muted"].clone());
    if slot("comment").is_some() {
        pick(&mut d, "ui.dim", "comment");
    } else {
        set(&mut d, "ui.dim", muted.0.clone(), muted.1.clone());
    }
    pick(
        &mut d,
        "ui.border",
        if slot("line").is_some() {
            "line"
        } else if slot("bg_alt").is_some() {
            "bg_alt"
        } else {
            "comment"
        },
    );
    if slot("comment").is_some() {
        pick(&mut d, "ui.border-strong", "comment");
    } else {
        let b = (d.tokens["ui.border"].clone(), d.source["ui.border"].clone());
        set(&mut d, "ui.border-strong", b.0, b.1);
    }

    // a mark color: the slot itself, else a same-hue palette color that reaches `need`
    // `solid`: the color is also a fill under text (buttons, answered question numbers), so the
    // background or foreground must read on it at 4.5:1
    let fill_ok = |c: &str| cr(&surface, c).max(cr(&fg, c)) >= 4.5;
    let first = |cands: &[&str], need: f64, avoid: &[String], solid: bool| -> Option<String> {
        for k in cands {
            if let Some(c) = slot(k) {
                if !avoid.contains(&c) && cr(&c, &surface) >= need && (!solid || fill_ok(&c)) {
                    return Some(k.to_string());
                }
            }
        }
        for k in cands {
            let Some(base) = slot(k) else { continue };
            let best = all
                .iter()
                .filter(|(_, c, _)| !avoid.contains(c) && cr(c, &surface) >= need && hs(c).1 >= 0.25 && (!solid || fill_ok(c)))
                .map(|(x, c, _)| (hue_dist(c, &base), x))
                .filter(|(dd, _)| *dd <= 20.0)
                .min_by(|a, b| a.0.total_cmp(&b.0));
            if let Some((_, x)) = best {
                return Some(x.clone());
            }
        }
        None
    };
    // text: the key's color if it reads on every bg, else the nearest-hue palette color that does, else fg
    // A substitute never takes another role's color or drifts toward another role's hue: it is a
    // shade of the same hue (20° or closer, and closer to this role than to any other).
    let text_for = |key: &str, bgs: &[String], others: &[String]| -> String {
        let ok = |c: &str| bgs.iter().all(|b| cr(c, b) >= 4.5);
        let Some(base) = get(key) else { return "fg".into() };
        if ok(&base) {
            return key.to_string();
        }
        all.iter()
            .filter(|(k, c, _)| !matches!(k.as_str(), "bg" | "bg_alt" | "bg_dark" | "line") && ok(c) && hs(c).1 >= 0.15)
            .filter(|(_, c, _)| !others.contains(c) && others.iter().all(|o| hue_dist(c, &base) < hue_dist(c, o)))
            .map(|(k, c, _)| (hue_dist(c, &base), k))
            .filter(|(dd, _)| *dd <= 20.0)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, k)| k.clone())
            .unwrap_or_else(|| "fg".into())
    };
    let color = |k: &str| if k == "fg" { fg.clone() } else { get(k).unwrap_or_else(|| fg.clone()) };
    let role = |r: &str| p.roles.get(r).cloned();

    // interaction: blue/cyan; button text = bg or fg, whichever reads better
    // interaction: blue/cyan that also carries button text at 4.5:1; else a neutral button (fg fill)
    let ik = role("interact").or_else(|| first(&["blue", "cyan"], 3.0, &[], true)).unwrap_or_else(|| "fg".into());
    pick(&mut d, "ui.interact", &ik);
    let interact = color(&ik);
    let on = |c: &str| if cr(&surface, c) >= cr(&fg, c) { "bg" } else { "fg" };
    pick(&mut d, "ui.on-interact", on(&interact));
    let sa = SELECTION_ALPHAS.iter().copied().find(|a| cr(&fg, &over(&interact, *a, &surface)) >= 4.5).unwrap_or(0);
    set(&mut d, "ui.selection", format!("{interact}{sa:02x}"), format!("{} at {}%", name(&ik), pct(sa)));

    // needs-me
    let mut ek = first(&["red", "orange"], 3.0, &[], false).unwrap_or_else(|| "fg".into());
    let mut ak = first(&["purple", "magenta", "blue"], 3.0, std::slice::from_ref(&interact), false).unwrap_or_else(|| "fg".into());
    if p.cvd {
        ek = "red".into();
        ak = if slot("blue").is_some_and(|b| cr(&b, &surface) >= 3.0 && b != interact) { "blue".into() } else { "magenta".into() };
    }
    if let Some(r) = role("error") {
        ek = r;
    }
    if let Some(r) = role("ask") {
        ak = r;
    }
    let used = vec![interact.clone(), color(&ak), color(&ek)];
    let brightest = HUES.iter().filter_map(|h| slot(h)).map(|c| lum(&c)).fold(0.0, f64::max);
    let mut order: Vec<String> = p.turn.clone();
    order.extend(["orange", "cyan", "green"].map(String::from));
    if !p.cvd {
        order.push("yellow".into());
    }
    let tk = role("turn")
        .or_else(|| {
            order
                .iter()
                .map(|k| first(&[k.as_str()], 3.0, &used, false).unwrap_or_else(|| k.clone()))
                .find(|k| get(k).is_some_and(|c| !used.contains(&c) && cr(&c, &surface) >= 3.0 && !(k == "yellow" && p.dark && lum(&c) >= brightest)))
        })
        .unwrap_or_else(|| "comment".into());
    pick(&mut d, "signal.error", &ek);
    pick(&mut d, "signal.ask", &ak);
    pick(&mut d, "signal.turn", &tk);
    let core = [interact.clone(), color(&ak), color(&ek), color(&tk)];
    let others = |own: &str| core.iter().filter(|c| c.as_str() != own).cloned().collect::<Vec<_>>();
    for (r, k) in [("ask", &ak), ("error", &ek), ("turn", &tk)] {
        // a visible chip (6 % or more) with text that reads on it and on the card; a clear chip
        // only when not even the foreground reads on any tint
        let base = color(k);
        let found = CHIP_ALPHAS.iter().copied().filter(|a| *a >= 0x0f).find_map(|a| {
            let tinted = over(&base, a, &surface);
            let txt = text_for(k, &[surface.clone(), tinted.clone()], &others(&base));
            (cr(&color(&txt), &tinted) >= 4.5 && cr(&color(&txt), &surface) >= 4.5).then_some((txt, a))
        });
        let (txt, a) = found.unwrap_or_else(|| {
            let a = CHIP_ALPHAS.iter().copied().find(|a| cr(&fg, &over(&base, *a, &surface)) >= 4.5).unwrap_or(0);
            ("fg".into(), a)
        });
        pick(&mut d, &format!("signal.{r}-text"), &txt);
        set(&mut d, &format!("signal.{r}-bg"), format!("{base}{a:02x}"), format!("{} at {}%", name(k), pct(a)));
    }
    let t = (d.tokens["signal.turn-text"].clone(), d.source["signal.turn-text"].clone());
    set(&mut d, "signal.decision", t.0, t.1);
    pick(&mut d, "ui.link", &text_for(&ik, std::slice::from_ref(&surface), &others(&interact)));

    // informational text
    let turn = color(&tk);
    let err = color(&ek);
    let wbase = ["yellow", "orange"].into_iter().find(|k| slot(k).is_some_and(|c| c != turn && c != err)).unwrap_or("fg");
    pick(&mut d, "info.warn", &text_for(wbase, std::slice::from_ref(&surface), &core));
    let hbase = if slot("green").is_some_and(|c| c != turn) {
        "green"
    } else if slot("cyan").is_some() {
        "cyan"
    } else {
        "fg"
    };
    pick(&mut d, "info.thread", &text_for(hbase, std::slice::from_ref(&surface), &core));
    for k in ["info.cache", "info.progress", "info.held", "harness.omp", "harness.cc", "harness.cdx", "harness.agy"] {
        set(&mut d, k, muted.0.clone(), muted.1.clone());
    }

    // timeline: working is a neutral that stays apart from the solid needs-me bars in grayscale
    let (ask_c, err_c) = (color(&ak), color(&ek));
    let separates = |c: &str| cr(&ask_c, c) >= 1.5 && cr(&err_c, c) >= 1.5 && cr(c, &surface) >= 1.3;
    let wk = ["comment", "fg_dim", "line"].into_iter().map(String::from).find(|k| slot(k).is_some_and(|c| separates(&c))).or_else(|| {
        let mut n: Vec<&(String, String, String)> = all.iter().filter(|(_, c, _)| hs(c).1 < 0.35).collect();
        n.sort_by(|a, b| lum(&a.1).total_cmp(&lum(&b.1)));
        if p.dark {
            n.reverse();
        }
        n.into_iter().find(|(_, c, _)| separates(c)).map(|x| x.0.clone())
    });
    let wk = wk.ok_or_else(|| format!("{}: no neutral color separates the working bar from the question and error bars", p.id))?;
    pick(&mut d, "timeline.working", &wk);
    for (t, from) in [("timeline.idle", "ui.border"), ("timeline.snoozed", "ui.dim"), ("timeline.now", "ui.interact")] {
        let v = (d.tokens[from].clone(), d.source[from].clone());
        set(&mut d, t, v.0, v.1);
    }
    // brand: the palette's orange (else red); pill = a neutral under the wordmark's white text
    pick(&mut d, "brand.accent", if slot("orange").is_some() { "orange" } else { "red" });
    let mut pills: Vec<&(String, String, String)> = all.iter().filter(|(_, c, _)| cr("#ffffff", c) >= 4.5 && hs(c).1 < 0.4).collect();
    pills.sort_by(|a, b| if p.dark { lum(&b.1).total_cmp(&lum(&a.1)) } else { lum(&a.1).total_cmp(&lum(&b.1)) });
    let pk = pills
        .iter()
        .find(|x| matches!(x.0.as_str(), "bg_dark" | "bg_alt" | "bg" | "line" | "fg" | "comment"))
        .or(pills.first())
        .map(|x| x.0.clone())
        .ok_or_else(|| format!("{}: no color dark enough for the wordmark pill", p.id))?;
    pick(&mut d, "brand.pill", &pk);
    d.needs_me = (ek, ak, tk);
    Ok(d)
}

/// Theme file text for derived tokens; comments name the palette color of each value. Strings
/// go through the TOML serializer; comment text is kept to one line.
pub fn to_toml(p: &Palette, d: &Derived, base_note: &str) -> String {
    let q = |v: &str| toml::Value::String(v.to_string()).to_string();
    let line = |v: &str| v.replace(['\n', '\r'], " ");
    let mut s = format!(
        "#:schema {}\n# {}{}\n# Source: {}\nname = {}\nfamily = {}\nlabel = {}\nbase = \"light\"\n",
        crate::theme::SCHEMA_URL,
        line(&p.label),
        line(base_note),
        line(&p.source),
        q(&p.id),
        q(&p.family),
        q(&p.label)
    );
    let mut group = "";
    for (k, v) in &d.tokens {
        let (g, key) = k.split_once('.').unwrap_or((k, ""));
        if g != group {
            group = g;
            s.push_str(&format!("\n[{g}]\n"));
        }
        let src = d.source.get(k).map(|x| format!("  # {}", line(x))).unwrap_or_default();
        s.push_str(&format!("{key} = {}{src}\n", q(v)));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// sRGB → simulated dichromat sRGB (Machado, Oliveira & Fernandes 2009, severity 1.0).
    fn simulate(c: &str, m: [[f64; 3]; 3]) -> (f64, f64, f64) {
        let lin = |v: f64| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        let (r, g, b) = rgb(c);
        let v = [lin(r), lin(g), lin(b)];
        let o: Vec<f64> = m.iter().map(|row| (row[0] * v[0] + row[1] * v[1] + row[2] * v[2]).clamp(0.0, 1.0)).collect();
        (o[0], o[1], o[2])
    }

    /// CIELAB distance of two linear-RGB colors (D65).
    fn delta_e(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
        let lab = |(r, g, bb): (f64, f64, f64)| {
            let x = (0.4124 * r + 0.3576 * g + 0.1805 * bb) / 0.95047;
            let y = 0.2126 * r + 0.7152 * g + 0.0722 * bb;
            let z = (0.0193 * r + 0.1192 * g + 0.9505 * bb) / 1.08883;
            let f = |t: f64| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
            (116.0 * f(y) - 16.0, 500.0 * (f(x) - f(y)), 200.0 * (f(y) - f(z)))
        };
        let (p, q) = (lab(a), lab(b));
        ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2) + (p.2 - q.2).powi(2)).sqrt()
    }

    #[test]
    fn okabe_ito_needs_me_colors_stay_apart_for_dichromats() {
        const PROTAN: [[f64; 3]; 3] = [[0.152286, 1.052583, -0.204868], [0.114503, 0.786281, 0.099216], [-0.003882, -0.048116, 1.051998]];
        const DEUTAN: [[f64; 3]; 3] = [[0.367322, 0.860646, -0.227968], [0.280085, 0.672501, 0.047413], [-0.011820, 0.042940, 0.968881]];
        for id in ["okabe-ito-dark", "okabe-ito-light"] {
            let src = crate::theme::PALETTES.iter().find(|s| s.contains(&format!("\"id\": \"{id}\""))).unwrap();
            let d = derive(&serde_json::from_str(src).unwrap()).unwrap();
            let c = ["signal.error", "signal.ask", "signal.turn"].map(|k| d.tokens[k].clone());
            for (m, name) in [(PROTAN, "protanopia"), (DEUTAN, "deuteranopia")] {
                for i in 0..3 {
                    for j in i + 1..3 {
                        let de = delta_e(simulate(&c[i], m), simulate(&c[j], m));
                        assert!(de >= 20.0, "{id} {name}: {} vs {} ΔE {de:.1}", c[i], c[j]);
                    }
                }
            }
        }
    }

    #[test]
    fn every_builtin_palette_derives_and_reads_the_official_values_only() {
        for src in crate::theme::PALETTES {
            let p: Palette = serde_json::from_str(src).unwrap();
            let d = derive(&p).unwrap_or_else(|e| panic!("{}: {e}", p.id));
            let official: Vec<String> = p
                .colors
                .values()
                .map(|x| x.0.to_ascii_lowercase())
                .chain(p.extra.values().filter_map(|v| v.as_str().map(|s| s.to_ascii_lowercase())))
                .collect();
            for (k, v) in &d.tokens {
                // a value is a palette color, or one at reduced alpha
                assert!(official.contains(&v[..7].to_string()), "{}: {k} = {v} is not a palette color", p.id);
            }
            assert_ne!(d.tokens["signal.turn"], d.tokens["signal.error"], "{}", p.id);
            // a text substitute never borrows another state's color (Dracula error text was my-turn orange)
            let core = |k: &str| d.tokens[k].clone();
            for (text, own) in
                [("signal.error-text", "signal.error"), ("signal.ask-text", "signal.ask"), ("signal.turn-text", "signal.turn"), ("ui.link", "ui.interact")]
            {
                for other in ["signal.error", "signal.ask", "signal.turn", "ui.interact"] {
                    // the neutral foreground may serve several roles (text fallback, neutral button)
                    if other != own && core(text) != d.tokens["ui.fg"] {
                        assert_ne!(core(text), core(other), "{}: {text} uses the {other} color", p.id);
                    }
                }
            }
            assert_ne!(d.tokens["signal.turn"], d.tokens["signal.ask"], "{}", p.id);
        }
    }
}
