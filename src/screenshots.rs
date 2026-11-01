//! Screenshot imports for mission prompts. Terminal drops arrive as pasted paths.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::sync::atomic::{AtomicU64, Ordering};

pub const MAX_ATTACHMENTS: usize = 8;
const MAX_BYTES: u64 = 20 * 1024 * 1024;
static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Attachment {
    pub name: String,
    pub path: String,
}

pub fn dropped_paths(text: &str) -> Option<Vec<PathBuf>> {
    let text = text.trim();
    let paths = if Path::new(text).is_file() {
        vec![text.to_string()]
    } else {
        shell_words::split(&text.replace("\r\n", "\n")).ok()?
    };
    if paths.is_empty() { return None; }
    paths.into_iter().map(|p| {
        let p = if let Some(uri) = p.strip_prefix("file://") {
            let local = uri.strip_prefix("localhost/").map(|p| format!("/{p}"));
            let uri = local.as_deref().unwrap_or(uri);
            if !uri.starts_with('/') { return None; }
            let mut bytes = Vec::new();
            let mut it = uri.bytes();
            while let Some(b) = it.next() {
                bytes.push(if b == b'%' {
                    let hi = (it.next()? as char).to_digit(16)?;
                    let lo = (it.next()? as char).to_digit(16)?;
                    (hi * 16 + lo) as u8
                } else { b });
            }
            String::from_utf8(bytes).ok()?
        } else { p };
        let path = if let Some(rest) = p.strip_prefix("~/") { crate::game::home().join(rest) } else { PathBuf::from(p) };
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif").then_some(path)
    }).collect()
}

fn image_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") { Some("png") }
    else if bytes.starts_with(b"\xff\xd8\xff") { Some("jpg") }
    else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") { Some("gif") }
    else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") { Some("webp") }
    else { None }
}

fn read_image(path: &Path) -> Result<Vec<u8>, String> {
    let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return Err("Screenshots must be regular image files under 20 MiB".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES || image_type(&bytes).is_none() {
        return Err(format!("{} is not a supported screenshot (PNG, JPEG, WebP or GIF)", path.display()));
    }
    Ok(bytes)
}

fn save(bytes: &[u8], name: String, dir: &Path) -> Result<Attachment, String> {
    let ext = image_type(bytes).ok_or("Clipboard does not contain a supported image")?;
    if bytes.len() as u64 > MAX_BYTES { return Err("Screenshot exceeds 20 MiB".into()); }
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|e| e.to_string())?;
    let dir = dir.canonicalize().map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let serial = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!("{stamp}-{}-{serial}.{ext}", std::process::id()));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).map_err(|e| e.to_string())?;
    if let Err(e) = file.write_all(bytes) {
        let _ = std::fs::remove_file(&path);
        return Err(e.to_string());
    }
    Ok(Attachment { name, path: path.to_string_lossy().into_owned() })
}

pub fn directory() -> PathBuf { crate::daemon::dirs_state().join("screenshots") }

pub fn import(paths: &[PathBuf], dir: &Path) -> Result<Vec<Attachment>, String> {
    if paths.len() > MAX_ATTACHMENTS { return Err("Attach up to 8 screenshots per mission".into()); }
    let mut attachments = Vec::new();
    for path in paths {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        match read_image(path).and_then(|bytes| save(&bytes, name, dir)) {
            Ok(image) => attachments.push(image),
            Err(e) => { discard(&attachments); return Err(e); }
        }
    }
    Ok(attachments)
}

/// Only called for copies owned by an unsent draft, never submitted missions.
pub fn discard(attachments: &[Attachment]) {
    for image in attachments { let _ = std::fs::remove_file(&image.path); }
}

pub fn validate(attachments: &[Attachment]) -> Result<(), String> {
    if attachments.len() > MAX_ATTACHMENTS { return Err("Attach up to 8 screenshots per mission".into()); }
    for image in attachments { read_image(Path::new(&image.path))?; }
    Ok(())
}

pub fn clipboard(dir: &Path) -> Result<Attachment, String> {
    save(&clipboard_bytes(true)?, "Clipboard screenshot.png".into(), dir)
}

pub enum Clipboard {
    Image(Attachment),
    Text(String),
}

/// Prefer an image when the clipboard offers both an image and a text description.
pub fn clipboard_content(dir: &Path) -> Result<Clipboard, String> {
    if let Ok(bytes) = clipboard_bytes(true) {
        return save(&bytes, "Clipboard screenshot.png".into(), dir).map(Clipboard::Image);
    }
    let bytes = clipboard_bytes(false)?;
    String::from_utf8(bytes).map(Clipboard::Text).map_err(|_| "Clipboard text is not UTF-8".into())
}

fn clipboard_bytes(image: bool) -> Result<Vec<u8>, String> {
    use std::process::{Command, Stdio};
    let (program, args): (&str, &[&str]) = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        ("wl-paste", if image { &["--no-newline", "--type", "image/png"] } else { &["--no-newline", "--type", "text"] })
    } else {
        ("xclip", if image { &["-selection", "clipboard", "-t", "image/png", "-o"] } else { &["-selection", "clipboard", "-t", "UTF8_STRING", "-o"] })
    };
    let mut child = Command::new(program).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
        .map_err(|_| format!("Install {program} for clipboard screenshots, or drop an image file"))?;
    let stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(MAX_BYTES + 1).read_to_end(&mut bytes).map(|_| bytes)
    });
    let start = std::time::Instant::now();
    let success = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if start.elapsed().as_secs_f32() < 2.0 => std::thread::sleep(std::time::Duration::from_millis(10)),
            _ => { let _ = child.kill(); let _ = child.wait(); break false; }
        }
    };
    let bytes = reader.join().map_err(|_| "Could not read clipboard")?.map_err(|e| e.to_string())?;
    if !success { return Err(if image { "No PNG screenshot on the clipboard; copy an image or drop a file" } else { "Could not paste clipboard text or image; use terminal paste or drop an image file" }.into()); }
    if bytes.len() as u64 > MAX_BYTES { return Err("Clipboard content exceeds 20 MiB".into()); }
    Ok(bytes)
}

pub fn prompt(text: &str, attachments: &[Attachment]) -> String {
    if attachments.is_empty() { return text.into(); }
    let files = serde_json::to_string(attachments).unwrap_or_default();
    format!("{text}\n\nAttached screenshots (local files): {files}\nOpen and inspect these images as context for this mission. If you cannot view images, say so before proceeding.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_quoted_escaped_and_uri_drops_without_eating_prose() {
        for input in ["'/tmp/screen shot.png' /tmp/b.jpg", "/tmp/screen\\ shot.png /tmp/b.jpg", "file:///tmp/screen%20shot.png\r\nfile://localhost/tmp/b.jpg\r\n"] {
            assert_eq!(dropped_paths(input), Some(vec![PathBuf::from("/tmp/screen shot.png"), PathBuf::from("/tmp/b.jpg")]));
        }
        assert_eq!(dropped_paths("Please fix the layout in this screenshot"), None);
        assert_eq!(dropped_paths("file://remote/tmp/a.png"), None);
    }

    #[test]
    fn imported_screenshots_survive_source_removal_and_bad_imports_roll_back() {
        let dir = std::env::temp_dir().join(format!("empire-screenshots-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("original.png");
        let bytes = b"\x89PNG\r\n\x1a\nfixture";
        std::fs::write(&source, bytes).unwrap();
        let store = dir.join("stored");
        let imported = import(&[source.clone()], &store).unwrap();
        std::fs::remove_file(&source).unwrap();
        assert_eq!(std::fs::read(&imported[0].path).unwrap(), bytes);
        assert_eq!(imported[0].name, "original.png");
        assert!(import(&[PathBuf::from(&imported[0].path), source], &store).is_err());
        assert_eq!(std::fs::read_dir(&store).unwrap().count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn prompts_reference_images_and_plain_prompts_stay_unchanged() {
        let image = Attachment { name: "screen shot.png".into(), path: "/saved/123.png".into() };
        let text = prompt("Fix this layout", &[image]);
        assert!(text.contains("Fix this layout"));
        assert!(text.contains("/saved/123.png"));
        assert!(text.contains("screenshot"));
        assert_eq!(prompt("plain mission", &[]), "plain mission");
    }
}
