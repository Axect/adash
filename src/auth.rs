//! Harness login screens: a terminal that waits for the user to sign in. Only phrases found in
//! the harnesses' own binaries are matched (Claude Code 2.1, Codex 0.164, Antigravity, omp), and
//! only on the last rows of the rendered screen, where a prompt waiting for input sits.
use serde_json::{json, Value};
use std::sync::LazyLock;

/// (harness, phrase, what it means). Matched case-sensitively, as the harness prints it, and only
/// for that harness (another agent's screen may quote these words).
const PHRASES: &[(&str, &str, &str)] = &[
    // Claude Code
    ("claude", "Please run /login", "Claude Code: sign in with /login"),
    ("claude", "Invalid API key", "Claude Code: invalid API key"),
    ("claude", "Paste code here", "Claude Code: paste the sign-in code"),
    ("claude", "Select login method", "Claude Code: choose a login method"),
    // Codex
    ("codex", "Please sign in again", "Codex: sign in again"),
    ("codex", "Couldn't load local sign-in", "Codex: sign in again"),
    // Antigravity
    ("antigravity", "You are not logged into Antigravity", "Antigravity: not signed in"),
    ("antigravity", "Authentication required. Please visit the URL to log in", "Antigravity: open the sign-in URL"),
    ("antigravity", "Log in with your Google account by pasting a code", "Antigravity: paste the sign-in code"),
    ("antigravity", "Sign-in timed out", "Antigravity: sign-in timed out"),
    ("antigravity", "You are currently not signed in", "Antigravity: not signed in"),
    ("antigravity", "Select login method", "Antigravity: choose a login method"),
    // omp
    ("omp", "signed out automatically. Run /login", "omp: sign in with /login"),
    ("omp", "Open this URL in your browser:", "omp: open the sign-in URL"),
    ("omp", "Paste the authorization code", "omp: paste the authorization code"),
];

static URL: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r#"https://[^\s"'<>`│]+"#).unwrap());
static CODE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\b[A-Z0-9]{4,5}-[A-Z0-9]{4,5}\b").unwrap());

/// Rows of the screen to look at: the last 15 non-empty ones.
const ROWS: usize = 15;

/// A login prompt on the screen: {reason, phrase, url?, code?}.
pub fn detect(kind: &str, screen: &[String]) -> Option<Value> {
    let rows: Vec<&str> = screen.iter().map(|l| l.as_str()).filter(|l| !l.trim().is_empty()).collect();
    let tail = &rows[rows.len().saturating_sub(ROWS)..];
    let text = tail.join("\n");
    let (_, phrase, reason) = PHRASES.iter().find(|(k, p, _)| *k == kind && text.contains(p))?;
    let url = URL.find(&text).map(|m| m.as_str().trim_end_matches(['.', ',', ')']).to_string());
    let code = CODE.find(&text).map(|m| m.as_str().to_string());
    Some(json!({"reason": reason, "phrase": phrase, "url": url, "code": code}))
}

/// Keys the terminal view may send by name (anything else goes as literal text).
pub const KEYS: &[&str] = &["Enter", "Escape", "Up", "Down", "Left", "Right", "Tab", "BTab", "C-c", "BSpace", "Space", "PageUp", "PageDown"];

/// (harness, question) of the first-run "trust this folder?" screens, as found on real screens
/// and in the binaries (Antigravity, Claude Code). The accept option must read exactly
/// [`TRUST_YES`].
const TRUST: &[(&str, &str)] = &[("antigravity", "Do you trust the contents of this project?"), ("claude", "Is this a project you created or one you trust?")];
pub const TRUST_YES: &str = "Yes, I trust this folder";

// an option row: optional cursor, optional "1." number, then the label (only the two known
// labels are taken as options, so other numbered text on the screen is not)
static OPTION: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^\s*(❯|›|>)?\s*(?:\d+\.\s+)?((?:Yes, I trust this folder|No, exit|No)\b.*?)\s*$").unwrap());

/// A trust screen: {question, options: [{n, label, selected}], yes, no} (yes/no = option index).
pub fn trust(kind: &str, screen: &[String]) -> Option<Value> {
    let (_, q) = TRUST.iter().find(|(k, q)| *k == kind && screen.iter().any(|l| l.contains(q)))?;
    let opts: Vec<(bool, String)> = screen.iter().filter_map(|l| OPTION.captures(l)).map(|c| (c.get(1).is_some(), c[2].to_string())).collect();
    let yes = opts.iter().position(|(_, l)| l.starts_with(TRUST_YES));
    let no = opts.iter().position(|(_, l)| l.starts_with("No"));
    Some(json!({
        "question": q, "yes": yes, "no": no,
        "options": opts.iter().enumerate().map(|(i, (s, l))| json!({"n": i, "label": l, "selected": s})).collect::<Vec<_>>(),
    }))
}

/// Arrow presses (`Up`/`Down` and how many) from the selected option to `target`.
pub fn moves(v: &Value, target: usize) -> Option<(&'static str, usize)> {
    let sel = v["options"].as_array()?.iter().position(|o| o["selected"] == true)?;
    Some(if target >= sel { ("Down", target - sel) } else { ("Up", sel - target) })
}

/// Input the harness took but holds until its turn ends, as its own screen says (phrases from
/// the binaries): (harness, phrase, tmux key that sends it now). omp shows its queue only in a
/// passing notice, so it is not listed.
const QUEUED: &[(&str, &str, &str)] = &[("antigravity", "to send immediately)", "M-m"), ("claude", "Press up to edit queued messages", "Enter")];

/// A held input on the screen's last rows: {phrase, sendNowKey}.
pub fn queued(kind: &str, screen: &[String]) -> Option<Value> {
    let rows: Vec<&str> = screen.iter().map(|l| l.as_str()).filter(|l| !l.trim().is_empty()).collect();
    let text = rows[rows.len().saturating_sub(ROWS)..].join("\n");
    let (_, p, k) = QUEUED.iter().find(|(h, p, _)| *h == kind && text.contains(p))?;
    Some(json!({"phrase": p, "sendNowKey": k}))
}

/// The turn was stopped by the user and the harness waits at an empty prompt: Antigravity prints
/// "Interrupted · What should Antigravity CLI do instead?" and leaves no finished model step, so
/// its conversation alone would still read as working.
pub fn interrupted(kind: &str, screen: &[String]) -> bool {
    if kind != "antigravity" {
        return false;
    }
    let rows: Vec<&str> = screen.iter().map(|l| l.as_str()).filter(|l| !l.trim().is_empty()).collect();
    let tail = &rows[rows.len().saturating_sub(ROWS)..];
    let stopped = tail.iter().any(|l| l.contains("Interrupted · What should Antigravity CLI do instead?"));
    let empty_prompt = tail.iter().any(|l| l.trim() == ">");
    stopped && empty_prompt
}

/// omp's confirmation box for a config change the agent asked for ("💾 Your agent wants to save
/// `x` to your config." / "⚙️ Your agent wants to change `x`…", from the binary): {question,
/// detail, options: [{n, label, selected}]}. Options are the rows between the detail and the
/// "↑/↓ navigate" hint.
pub fn approval(kind: &str, screen: &[String]) -> Option<Value> {
    if kind != "omp" {
        return None;
    }
    let top = screen.iter().rposition(|l| l.starts_with('╭') && l.contains("Your agent wants to "))?;
    let q = screen[top].trim_start_matches(['╭', '─', ' ']).trim_end_matches(['─', '╮', ' ']).trim().to_string();
    let body: Vec<&str> = screen[top + 1..].iter().take_while(|l| !l.starts_with('╰')).filter_map(|l| l.strip_prefix('│')).map(|l| l.trim_end().trim_end_matches('│')).collect();
    let hint = body.iter().position(|l| l.contains("navigate"))?;
    let rows: Vec<&str> = body[..hint].iter().copied().filter(|l| !l.trim().is_empty()).collect();
    let detail = rows.first()?.trim().to_string();
    let opts: Vec<Value> = rows[1..]
        .iter()
        .enumerate()
        .map(|(n, l)| {
            let t = l.trim_start();
            let first = t.chars().next().unwrap_or(' ');
            let selected = first == '>' || first == '❯' || ('\u{e000}'..='\u{f8ff}').contains(&first);
            let label = if selected { t.chars().skip(1).collect::<String>().trim().to_string() } else { t.trim().to_string() };
            json!({"n": n, "label": label, "selected": selected})
        })
        .collect();
    (!opts.is_empty()).then(|| json!({"question": q, "detail": detail, "options": opts}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn login_prompts_are_found_on_the_last_rows_only() {
        let v = detect(
            "antigravity",
            &s(&["Authentication required. Please visit the URL to log in:", "https://accounts.google.com/o/oauth2/auth?client_id=x&state=y", "Enter code: "]),
        )
        .unwrap();
        assert_eq!(v["url"], "https://accounts.google.com/o/oauth2/auth?client_id=x&state=y");
        assert_eq!(v["reason"], "Antigravity: open the sign-in URL");
        let v = detect("claude", &s(&["  Session expired. Please run /login to sign in again.", "", "> "])).unwrap();
        assert!(v["url"].is_null());
        assert_eq!(
            detect("omp", &s(&["Go to https://example.com/device and enter WDJB-MJHT", "Paste the authorization code (or full redirect URL):"])).unwrap()
                ["code"],
            "WDJB-MJHT"
        );
        // an ordinary screen, or the phrase scrolled far up: nothing
        assert!(detect("omp", &s(&["> working", "done"])).is_none());
        let mut old = s(&["Please run /login"]);
        old.extend((0..20).map(|i| format!("line {i}")));
        assert!(detect("claude", &old).is_none());
        // another harness quoting the phrase (an omp agent discussing Claude Code): nothing
        assert!(detect("omp", &s(&["Please run /login"])).is_none());
    }

    #[test]
    fn trust_screens_and_the_way_to_the_option() {
        let agy = s(&[" Do you trust the contents of this project?", " /tmp/x", " > 1. No, exit", "   2. Yes, I trust this folder"]);
        let v = trust("antigravity", &agy).unwrap();
        assert_eq!((v["yes"].as_u64(), v["no"].as_u64()), (Some(1), Some(0)));
        assert_eq!(moves(&v, 1), Some(("Down", 1)));
        let claude = s(&["Quick safety check: Is this a project you created or one you trust?", " ❯ 1. Yes, I trust this folder", "   2. No, exit"]);
        let v = trust("claude", &claude).unwrap();
        assert_eq!((v["yes"].as_u64(), moves(&v, 0)), (Some(0), Some(("Down", 0))));
        assert!(trust("omp", &claude).is_none(), "only the harness's own screen");
        // the real screens (2026-11-08): no numbers
        let agy = s(&[
            "Do you trust the contents of this project?",
            "Antigravity CLI requires permission to read, edit, and execute files here.",
            "> Yes, I trust this folder",
            "  No, exit",
            "  ↑/↓ Navigate · enter Confirm",
        ]);
        let v = trust("antigravity", &agy).unwrap();
        assert_eq!((v["yes"].as_u64(), v["no"].as_u64(), moves(&v, 0)), (Some(0), Some(1), Some(("Down", 0))));
        let cc = s(&[
            " Quick safety check: Is this a project you created or one you trust? (Like your own code",
            " Security guide",
            " ❯ No, exit",
            "   Yes, I trust this folder",
            " Enter to confirm · Esc to cancel",
        ]);
        let v = trust("claude", &cc).unwrap();
        assert_eq!((v["yes"].as_u64(), moves(&v, 1)), (Some(1), Some(("Down", 1))));
    }

    #[test]
    fn held_input_per_harness() {
        assert_eq!(queued("antigravity", &s(&["> working", "뭐하는중이야 (alt+m to send immediately)"])).unwrap()["sendNowKey"], "M-m");
        assert!(queued("claude", &s(&["(alt+m to send immediately)"])).is_none());
        assert_eq!(queued("claude", &s(&["  > follow up", "Press up to edit queued messages"])).unwrap()["sendNowKey"], "Enter");
    }

    #[test]
    fn agy_interrupted_at_an_empty_prompt() {
        let scr = s(&["  생각은 어때?", "  ⎿  Interrupted · What should Antigravity CLI do instead?", "────", ">", "────", "press ctrl+c again to exit"]);
        assert!(interrupted("antigravity", &scr));
        let typed = s(&["  ⎿  Interrupted · What should Antigravity CLI do instead?", "> next instruction"]);
        assert!(!interrupted("antigravity", &typed), "something typed: a new turn is coming");
        assert!(!interrupted("omp", &scr));
    }

    #[test]
    fn omp_config_change_approval() {
        let scr = s(&[
            "  \u{f12b7} Persisting browser.enabled=false",
            "╭─ 💾 Your agent wants to save `browser.enabled` to your config. ─────",
            "│",
            "│ false → false                    │",
            "│",
            "│    Always for this session",
            "│  \u{f054} Allow once",
            "│    Deny",
            "│",
            "│ ↑/↓ navigate  \u{f0311} select  \u{f12b7} cancel",
            "╰──────",
            " ⠇ 13h · Opus 5.5",
        ]);
        let v = approval("omp", &scr).unwrap();
        assert_eq!(v["detail"], "false → false");
        let labels: Vec<&str> = v["options"].as_array().unwrap().iter().map(|o| o["label"].as_str().unwrap()).collect();
        assert_eq!(labels, vec!["Always for this session", "Allow once", "Deny"]);
        assert_eq!(moves(&v, 2), Some(("Down", 1)));
        assert!(approval("claude", &scr).is_none());
    }
}
