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
}
