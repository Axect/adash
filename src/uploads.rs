//! Chat uploads (images, PDFs, any file): durable files under
//! `~/.local/share/adash/uploads/YYYYMMDD/<HHMMSS>-<sha10>/<name>`, referenced from messages by
//! absolute path (agents open them with their own read tool). Files are data: stored 0600 in
//! 0700 folders, never executable, whatever their extension.
use crate::util::atomic_write;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const MAX_BYTES: usize = 100 * 1024 * 1024;
const KEEP_DAYS: u64 = 30;

pub fn dir() -> PathBuf {
    crate::platform::data_dir().join("uploads")
}

/// Extension for a supported image MIME type.
fn ext_for(mime: &str) -> Option<&'static str> {
    match mime.split(';').next().unwrap_or("").trim() {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

/// Do the leading bytes match the declared image type?
pub fn magic_ok(mime: &str, b: &[u8]) -> bool {
    match ext_for(mime) {
        Some("png") => b.starts_with(b"\x89PNG\r\n\x1a\n"),
        Some("jpg") => b.starts_with(&[0xFF, 0xD8, 0xFF]),
        Some("gif") => b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a"),
        Some("webp") => b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP",
        _ => false,
    }
}

/// File name safe to store and to quote in a message: the last path component, without control
/// characters, path separators, `..` and leading dots; whitespace becomes `_` (an `@path`
/// mention ends at a space); at most 120 bytes, keeping the extension. Empty → `file`.
pub fn sanitize_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut n: String = base
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| {
            if c.is_whitespace() {
                '_'
            } else if matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '-'
            } else {
                c
            }
        })
        .collect();
    while n.contains("..") {
        n = n.replace("..", ".");
    }
    let n = n.trim_start_matches(['.', '-', '_']).trim_end_matches(['.', ' ']).to_string();
    if n.is_empty() {
        return "file".into();
    }
    if n.len() <= 120 {
        return n;
    }
    let (stem, ext) = match n.rsplit_once('.') {
        Some((s, e)) if e.len() <= 10 && !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (n.clone(), String::new()),
    };
    let mut cut = 120 - ext.len();
    while !stem.is_char_boundary(cut.min(stem.len())) {
        cut -= 1;
    }
    format!("{}{ext}", &stem[..cut.min(stem.len())])
}

/// What the agent will be reading: image | pdf | text | archive | other (by content, then name).
pub fn kind_of(bytes: &[u8], name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|x| x.1.to_ascii_lowercase()).unwrap_or_default();
    if ["image/png", "image/jpeg", "image/gif", "image/webp"].iter().any(|m| magic_ok(m, bytes)) {
        return "image";
    }
    if bytes.starts_with(b"%PDF-") {
        return "pdf";
    }
    let archive = bytes.starts_with(b"PK\x03\x04")
        || bytes.starts_with(&[0x1f, 0x8b])
        || bytes.starts_with(b"7z\xbc\xaf\x27\x1c")
        || bytes.starts_with(b"\xfd7zXZ\0")
        || bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
        || bytes.starts_with(b"BZh")
        || bytes.starts_with(b"Rar!")
        || (bytes.len() > 262 && &bytes[257..262] == b"ustar");
    if archive {
        // office documents and EPUB are zip containers, not archives to unpack
        return if matches!(ext.as_str(), "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp" | "epub") { "other" } else { "archive" };
    }
    let head = &bytes[..bytes.len().min(8192)];
    let text_like = !head.contains(&0) && (std::str::from_utf8(head).is_ok() || std::str::from_utf8(&head[..head.len().saturating_sub(4)]).is_ok());
    if text_like {
        return "text";
    }
    "other"
}

fn mime_guess(kind: &str, name: &str, declared: &str) -> String {
    let declared = declared.split(';').next().unwrap_or("").trim();
    if !declared.is_empty() && declared != "application/octet-stream" {
        return declared.to_string();
    }
    let ext = name.rsplit_once('.').map(|x| x.1.to_ascii_lowercase()).unwrap_or_default();
    match (kind, ext.as_str()) {
        ("pdf", _) => "application/pdf",
        ("text", "md" | "markdown") => "text/markdown",
        ("text", "csv") => "text/csv",
        ("text", "json") => "application/json",
        ("text", _) => "text/plain",
        ("archive", "zip") => "application/zip",
        ("archive", "gz" | "tgz") => "application/gzip",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// Store a file; identical content (same sha256) reuses the stored copy. `name` is the browser's
/// file name (sanitized here); without one an image keeps its old `image.<ext>` name.
pub fn save(bytes: &[u8], mime: &str, name: Option<&str>) -> Result<Value, (u16, String)> {
    save_in(&dir(), bytes, mime, name)
}

fn save_in(root: &Path, bytes: &[u8], mime: &str, name: Option<&str>) -> Result<Value, (u16, String)> {
    if bytes.is_empty() {
        return Err((400, "empty body".into()));
    }
    if bytes.len() > MAX_BYTES {
        return Err((413, format!("file larger than {} MB", MAX_BYTES / 1024 / 1024)));
    }
    // a declared image must really be one (the browser shows it inline)
    if let Some(ext) = ext_for(mime) {
        if !magic_ok(mime, bytes) {
            return Err((400, format!("content is not a valid {}", mime.split(';').next().unwrap_or(mime))));
        }
        if name.is_none() {
            return save_named(root, bytes, mime, &format!("image.{ext}"));
        }
    }
    let generated;
    let name = match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => n,
        None => {
            // a paste without a file name: pasted-<time>.<extension of the type>
            let ext = match mime.split(';').next().unwrap_or("").trim() {
                "application/pdf" => "pdf",
                "text/markdown" => "md",
                "text/csv" => "csv",
                "application/json" => "json",
                "application/zip" => "zip",
                m if m.starts_with("text/") => "txt",
                _ => match kind_of(bytes, "") {
                    "pdf" => "pdf",
                    "text" => "txt",
                    _ => "bin",
                },
            };
            generated = format!("pasted-{}.{ext}", chrono::Local::now().format("%Y%m%d-%H%M%S"));
            &generated
        }
    };
    save_named(root, bytes, mime, &sanitize_name(name))
}

fn save_named(root: &Path, bytes: &[u8], mime: &str, name: &str) -> Result<Value, (u16, String)> {
    let sha = format!("{:x}", Sha256::digest(bytes));
    let tag = format!("-{}", &sha[..10]);
    // same content uploaded before (any name): reuse it
    let existing = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|d| std::fs::read_dir(d.path()).ok())
        .flatten()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().ends_with(&tag)) && p.is_dir())
        .and_then(|d| std::fs::read_dir(d).ok()?.flatten().map(|e| e.path()).find(|p| p.is_file()));
    let path = match existing {
        Some(p) => p,
        None => {
            let now = chrono::Local::now();
            let day = root.join(now.format("%Y%m%d").to_string());
            let folder = day.join(format!("{}{tag}", now.format("%H%M%S")));
            let p = folder.join(name);
            atomic_write(&p, bytes).map_err(|e| (500, e))?;
            private(&p, 0o600);
            for d in [root, day.as_path(), folder.as_path()] {
                private(d, 0o700);
            }
            p
        }
    };
    let stored = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let kind = kind_of(bytes, &stored);
    Ok(json!({"path": path.to_string_lossy(), "name": stored, "bytes": bytes.len(), "mime": mime_guess(kind, &stored, mime), "sha256": sha, "kind": kind}))
}

#[cfg(unix)]
fn private(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode));
}
#[cfg(not(unix))]
fn private(_: &Path, _: u32) {}

/// Canonical path if `p` is an existing file inside the uploads dir.
pub fn check_upload_path(p: &str) -> Result<PathBuf, String> {
    let root = dir().canonicalize().map_err(|_| "no uploads yet".to_string())?;
    let c = Path::new(p).canonicalize().map_err(|_| format!("upload not found: {p}"))?;
    if !c.starts_with(&root) || !c.is_file() {
        return Err(format!("attachments must be adash uploads: {p}"));
    }
    Ok(c)
}

/// Message text, then one line per image, then one line per file. omp and Claude Code take an
/// `@path` mention (the file is read into context, also mid-message); Codex and Antigravity get
/// the plain path with a request to read it.
pub fn compose_all(text: &str, images: &[PathBuf], files: &[PathBuf], agent_type: &str) -> String {
    let mut out = text.trim_end().to_string();
    let mut line = |s: String| {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&s);
    };
    for (k, p) in images.iter().enumerate() {
        line(format!("[첨부 이미지 {}] {} — 읽기 도구로 이 이미지를 열어 확인하세요.", k + 1, p.display()));
    }
    let mention = matches!(agent_type, "omp" | "claude");
    for (k, p) in files.iter().enumerate() {
        line(if mention {
            format!("[첨부 파일 {}] @{}", k + 1, p.display())
        } else {
            format!("[첨부 파일 {}] {} — 이 파일을 읽어 주세요.", k + 1, p.display())
        });
    }
    out
}

/// Remove uploads older than 30 days (and emptied folders).
pub fn cleanup() -> usize {
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(KEEP_DAYS * 86_400);
    let old = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).is_ok_and(|t| t < cutoff);
    let mut n = 0;
    for day in std::fs::read_dir(dir()).into_iter().flatten().flatten() {
        for e in std::fs::read_dir(day.path()).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                // <HHMMSS>-<sha10>/<name>
                for f in std::fs::read_dir(&p).into_iter().flatten().flatten() {
                    if old(&f.path()) && std::fs::remove_file(f.path()).is_ok() {
                        n += 1;
                    }
                }
                let _ = std::fs::remove_dir(&p);
            } else if old(&p) && std::fs::remove_file(&p).is_ok() {
                n += 1;
            }
        }
        let _ = std::fs::remove_dir(day.path()); // only succeeds when empty
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_bytes_must_match_declared_type() {
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
        assert!(magic_ok("image/png", png));
        assert!(!magic_ok("image/jpeg", png));
        assert!(magic_ok("image/jpeg", &[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10]));
        assert!(magic_ok("image/gif", b"GIF89a...."));
        assert!(magic_ok("image/webp", b"RIFF\x10\0\0\0WEBPVP8 "));
        assert!(!magic_ok("image/webp", b"RIFF\x10\0\0\0WAVEfmt "));
        assert!(!magic_ok("image/png", b"<svg xmlns=..."));
        assert!(!magic_ok("image/svg+xml", b"<svg/>"));
        assert_eq!(save(b"<html>", "image/png", None).unwrap_err().0, 400);
        assert!(save_in(&std::env::temp_dir().join(format!("adash-up-html-{}", std::process::id())), b"<p>x</p>", "text/html", None)
            .is_ok_and(|v| v["name"].as_str().unwrap().starts_with("pasted-")));
    }

    #[test]
    fn only_files_inside_the_upload_dir_are_accepted() {
        assert!(check_upload_path("/etc/hostname").is_err());
        assert!(check_upload_path(&format!("{}/../../../../etc/hostname", dir().display())).is_err());
        assert!(check_upload_path("/nonexistent/x.png").is_err());
    }

    #[test]
    fn compose_puts_text_first_then_one_line_per_image() {
        let m = compose_all("이 그림 봐 주세요", &[PathBuf::from("/u/a.png"), PathBuf::from("/u/b.jpg")], &[], "omp");
        assert_eq!(m, "이 그림 봐 주세요\n[첨부 이미지 1] /u/a.png — 읽기 도구로 이 이미지를 열어 확인하세요.\n[첨부 이미지 2] /u/b.jpg — 읽기 도구로 이 이미지를 열어 확인하세요.");
        assert!(compose_all("", &[PathBuf::from("/u/a.png")], &[], "codex").starts_with("[첨부 이미지 1]"));
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("adash-uploads-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn names_are_sanitized_but_recognizable() {
        assert_eq!(sanitize_name("Annual Report 2026.pdf"), "Annual_Report_2026.pdf");
        assert_eq!(sanitize_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_name("C:\\Users\\me\\논문 초안.docx"), "논문_초안.docx");
        assert_eq!(sanitize_name("..hidden..sh"), "hidden.sh");
        assert_eq!(sanitize_name("a\u{0}b\nc\u{7f}.txt"), "abc.txt");
        assert_eq!(sanitize_name("/"), "file");
        assert_eq!(sanitize_name("..."), "file");
        let long = format!("{}.pdf", "가".repeat(100));
        let s = sanitize_name(&long);
        assert!(s.len() <= 120 && s.ends_with(".pdf"), "{s}");
    }

    #[test]
    fn files_are_stored_as_private_data_and_reused_by_hash() {
        let root = tmp("save");
        let v = save_in(&root, b"%PDF-1.7\n...", "application/pdf", Some("My Paper.pdf")).unwrap();
        let p = PathBuf::from(v["path"].as_str().unwrap());
        assert_eq!((v["name"].as_str(), v["kind"].as_str(), v["mime"].as_str()), (Some("My_Paper.pdf"), Some("pdf"), Some("application/pdf")));
        assert!(p.parent().unwrap().file_name().unwrap().to_string_lossy().ends_with(&v["sha256"].as_str().unwrap()[..10]));
        // a script is just data: stored, never executable
        let sh = save_in(&root, b"#!/bin/sh\nrm -rf ~\n", "", Some("run.sh")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(sh["path"].as_str().unwrap()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert_eq!(sh["kind"], "text");
        // the same bytes under another name: the stored copy comes back
        let again = save_in(&root, b"%PDF-1.7\n...", "", Some("copy.pdf")).unwrap();
        assert_eq!(again["path"], v["path"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn oversize_is_refused_and_nameless_uploads_get_a_name() {
        let root = tmp("big");
        let big = vec![b'a'; MAX_BYTES + 1];
        assert_eq!(save_in(&root, &big, "text/plain", Some("big.txt")).unwrap_err().0, 413);
        let pasted = save_in(&root, b"%PDF-1.4\n", "application/pdf", None).unwrap();
        assert!(pasted["name"].as_str().unwrap().starts_with("pasted-") && pasted["name"].as_str().unwrap().ends_with(".pdf"), "{pasted}");
        assert_eq!(kind_of(b"PK\x03\x04....", "a.zip"), "archive");
        assert_eq!(kind_of(b"PK\x03\x04....", "a.docx"), "other");
        assert_eq!(kind_of(&[0, 1, 2, 255], "a.bin"), "other");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn files_are_mentioned_per_harness() {
        let f = [PathBuf::from("/u/20261103/101500-ab/paper.pdf")];
        assert_eq!(compose_all("요약해 줘", &[], &f, "omp"), "요약해 줘\n[첨부 파일 1] @/u/20261103/101500-ab/paper.pdf");
        assert_eq!(compose_all("", &[], &f, "claude"), "[첨부 파일 1] @/u/20261103/101500-ab/paper.pdf");
        assert_eq!(compose_all("x", &[], &f, "codex"), "x\n[첨부 파일 1] /u/20261103/101500-ab/paper.pdf — 이 파일을 읽어 주세요.");
    }
}
