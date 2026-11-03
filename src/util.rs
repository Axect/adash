use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub type Res<T> = Result<T, String>;

pub const DEFAULT_PORT: u16 = 7878;

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

pub fn state_dir() -> PathBuf {
    crate::platform::state_dir()
}

pub fn threads_dir() -> PathBuf {
    crate::platform::data_dir().join("threads")
}

pub fn meta_path() -> PathBuf {
    crate::platform::config_dir().join("meta.yaml")
}

/// Write via tmp file + rename so readers never see a partial file.
pub fn atomic_write(path: &Path, data: &[u8]) -> Res<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    }
    // unique per call: two threads writing the same file must not share a temp file
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_file_name(format!(".{}.tmp{}-{n}", path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(), std::process::id()));
    std::fs::write(&tmp, data).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))
}

/// Process output may carry NUL bytes, invalid UTF-8 and lone UTF-16 surrogate escapes
/// (text truncated between the halves of a `\udb80\udd4d` pair), all of which break serde_json.
pub fn sanitize(bytes: &[u8]) -> String {
    let cleaned: Vec<u8> = bytes.iter().copied().filter(|b| *b != 0).collect();
    fix_surrogates(&String::from_utf8_lossy(&cleaned))
}

/// Replace unpaired `\uD800`–`\uDFFF` escapes in JSON text with `\uFFFD`.
pub fn fix_surrogates(s: &str) -> String {
    let b = s.as_bytes();
    let hex4 = |i: usize| -> Option<u32> { s.get(i..i + 4).and_then(|h| u32::from_str_radix(h, 16).ok()) };
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut last = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            if b.get(i + 1) == Some(&b'u') {
                if let Some(u) = hex4(i + 2) {
                    if (0xD800..0xDC00).contains(&u) {
                        let low = (b.get(i + 6) == Some(&b'\\') && b.get(i + 7) == Some(&b'u')).then(|| hex4(i + 8)).flatten();
                        if low.is_some_and(|l| (0xDC00..0xE000).contains(&l)) {
                            i += 12;
                            continue;
                        }
                    }
                    if (0xD800..0xE000).contains(&u) {
                        out.push_str(&s[last..i]);
                        out.push_str("\\uFFFD");
                        i += 6;
                        last = i;
                        continue;
                    }
                }
            }
            i += 2; // skip any escaped char, incl. `\\`
            continue;
        }
        i += 1;
    }
    out.push_str(&s[last..]);
    out
}

pub fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    truncate(line, max)
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

pub fn port() -> u16 {
    std::env::var("ADASH_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(crate::config::get().port)
}

/// Minimal HTTP/1.1 client for talking to a running `adash serve`.
pub fn http(method: &str, path: &str, body: Option<&serde_json::Value>) -> Res<(u16, serde_json::Value)> {
    let addr = format!("127.0.0.1:{}", port());
    let mut s = TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_millis(300)).map_err(|e| format!("connect {addr}: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(60))).ok();
    let payload = body.map(|b| b.to_string()).unwrap_or_default();
    let path: String =
        path.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Type: application/json\r\nX-Adash: 1\r\nContent-Length: {}\r\n\r\n{payload}",
        payload.len()
    );
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&buf);
    let (head, body) = text.split_once("\r\n\r\n").ok_or("bad http response")?;
    let code: u16 = head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).ok_or("bad status")?;
    let body = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") { dechunk(body) } else { body.to_string() };
    let json = serde_json::from_str(&body).unwrap_or(serde_json::Value::String(body));
    Ok((code, json))
}

fn dechunk(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some((size, tail)) = rest.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 || tail.len() < n {
            break;
        }
        out.push_str(&tail[..n]);
        rest = tail[n..].trim_start_matches("\r\n");
    }
    out
}

pub fn server_up() -> bool {
    let addr = format!("127.0.0.1:{}", port());
    TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_millis(200)).is_ok()
}

/// "2026-10-28T05-55-25-977Z_<uuid>.jsonl" → epoch ms.
pub fn omp_file_created_ms(name: &str) -> Option<i64> {
    let ts = name.split('_').next()?;
    let ts = ts.strip_suffix('Z')?;
    // 2026-10-28T05-55-25-977
    let (date, time) = ts.split_once('T')?;
    let p: Vec<&str> = time.split('-').collect();
    if p.len() < 3 {
        return None;
    }
    let ms = p.get(3).copied().unwrap_or("0");
    let iso = format!("{date}T{}:{}:{}.{ms}Z", p[0], p[1], p[2]);
    chrono::DateTime::parse_from_rfc3339(&iso).ok().map(|d| d.timestamp_millis())
}

pub fn mtime_ms(p: &Path) -> i64 {
    std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::fix_surrogates;

    #[test]
    fn lone_surrogates_become_replacement_and_pairs_survive() {
        // text truncated in the middle of a glyph pair
        let raw = r#"{"a":"x \udb80","b":"\udb80\udd4d ok","c":"\\udb80 literal","d":"\udd4d"}"#;
        let fixed = fix_surrogates(raw);
        let v: serde_json::Value = serde_json::from_str(&fixed).expect("parses after repair");
        assert_eq!(v["a"], "x \u{FFFD}");
        assert_eq!(v["b"], "\u{F014D} ok");
        assert_eq!(v["c"], "\\udb80 literal");
        assert_eq!(v["d"], "\u{FFFD}");
        assert!(serde_json::from_str::<serde_json::Value>(raw).is_err());
    }
}

/// East-Asian wide / fullwidth characters (two terminal columns).
pub fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F300..=0x1F64F | 0x1F900..=0x1F9FF | 0x20000..=0x3FFFD)
}

/// Lock that survives a panic in another holder (poisoned mutex → take the data anyway), so one
/// panic does not turn every later request into a panic.
pub trait LockExt<T> {
    fn plock(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> LockExt<T> for std::sync::Mutex<T> {
    fn plock(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Before saving a config file: if the file on disk exists but does not parse, keep a copy
/// (`<file>.bad-<ms>`) and refuse to save — defaults loaded after a parse error must never
/// overwrite the user's file.
pub fn guard_config(path: &std::path::Path, parses: impl Fn(&str) -> bool) -> Res<()> {
    let Ok(text) = std::fs::read_to_string(path) else { return Ok(()) };
    if text.trim().is_empty() || parses(&text) {
        return Ok(());
    }
    let bad = path.with_file_name(format!("{}.bad-{}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), now_ms()));
    let _ = std::fs::copy(path, &bad);
    Err(format!("{} does not parse; not overwriting it (copy at {}). Fix or remove it.", path.display(), bad.display()))
}

#[cfg(test)]
mod guard_tests {
    #[test]
    fn unparseable_config_is_kept_and_not_overwritten() {
        let d = std::env::temp_dir().join(format!("adash-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("meta.yaml");
        std::fs::write(&f, "projects: [unclosed").unwrap();
        let r = super::guard_config(&f, |t| serde_yaml::from_str::<serde_yaml::Value>(t).is_ok());
        assert!(r.is_err());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "projects: [unclosed");
        assert!(std::fs::read_dir(&d).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with("meta.yaml.bad-")));
        std::fs::write(&f, "projects: {}").unwrap();
        assert!(super::guard_config(&f, |t| serde_yaml::from_str::<serde_yaml::Value>(t).is_ok()).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod atomic_tests {
    #[test]
    fn concurrent_writes_never_tear_or_fail() {
        let f = std::env::temp_dir().join(format!("adash-aw-{}.json", std::process::id()));
        let hs: Vec<_> = (0..8)
            .map(|i| {
                let f = f.clone();
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        super::atomic_write(&f, format!("{i}").repeat(1000).as_bytes()).unwrap();
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        let t = std::fs::read_to_string(&f).unwrap();
        assert_eq!(t.len(), 1000);
        assert!(t.chars().all(|c| c == t.chars().next().unwrap()), "one writer's whole content");
        let _ = std::fs::remove_file(&f);
    }
}
