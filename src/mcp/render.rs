//! A `tools/call` result as the CLI prints it, and the exit code it maps to.
//!
//! stdout is `structuredContent` as JSON when the tool sent it, else the text
//! blocks joined by newlines. Images, audio and binary resources are decoded to
//! files in `blob_dir` and their paths are printed (on stdout after the text,
//! or on stderr when stdout is JSON that a pipe will parse). A tool that
//! answered `isError` prints its text on stderr and exits 1, so `| jq` never
//! reads an error message as data.

use std::path::Path;

use serde_json::Value;

/// Exit codes (docs/SPEC.md, *MCP as a CLI*).
pub const EXIT_OK: i32 = 0;
pub const EXIT_TOOL_ERROR: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_UNAVAILABLE: i32 = 3;
pub const EXIT_TIMEOUT: i32 = 4;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rendered {
    pub stdout: String,
    pub stderr: String,
    pub is_error: bool,
    /// Files written for image/audio/binary content.
    pub saved: Vec<String>,
}

impl Rendered {
    pub fn exit_code(&self) -> i32 {
        if self.is_error {
            EXIT_TOOL_ERROR
        } else {
            EXIT_OK
        }
    }
}

/// Decode standard or URL-safe base64, ignoring whitespace and padding.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\r' | b'\n' | b' ' | b'\t' => continue,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn extension(mime: &str) -> &'static str {
    match mime.split(';').next().unwrap_or("").trim() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/ogg" => "ogg",
        "application/pdf" => "pdf",
        "application/json" => "json",
        "text/plain" => "txt",
        _ => "bin",
    }
}

fn save_blob(
    blob_dir: &Path,
    index: usize,
    mime: &str,
    data: &str,
    notes: &mut Vec<String>,
) -> Option<String> {
    let Some(bytes) = base64_decode(data) else {
        notes.push(format!(
            "content block {index}: the data is not valid base64"
        ));
        return None;
    };
    if let Err(error) = std::fs::create_dir_all(blob_dir) {
        notes.push(format!("cannot create {}: {error}", blob_dir.display()));
        return None;
    }
    let path = blob_dir.join(format!("{index}.{}", extension(mime)));
    match std::fs::write(&path, bytes) {
        Ok(()) => Some(crate::util::path_string(&path)),
        Err(error) => {
            notes.push(format!("cannot write {}: {error}", path.display()));
            None
        }
    }
}

/// Render one result. Never fails: a blob that cannot be written becomes a
/// note on stderr.
pub fn render(result: &Value, blob_dir: &Path, envelope: bool) -> Rendered {
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if envelope {
        return Rendered {
            stdout: result.to_string(),
            stderr: String::new(),
            is_error,
            saved: Vec::new(),
        };
    }
    let mut texts: Vec<String> = Vec::new();
    let mut saved: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let blocks = result
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (index, block) in blocks.iter().enumerate() {
        let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "text" => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    texts.push(text.to_string());
                }
            }
            "image" | "audio" => {
                let mime = block
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream");
                if let Some(data) = block.get("data").and_then(Value::as_str)
                    && let Some(path) = save_blob(blob_dir, index, mime, data, &mut notes)
                {
                    saved.push(path);
                }
            }
            "resource" => {
                let resource = block.get("resource").cloned().unwrap_or(Value::Null);
                let mime = resource
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream");
                if let Some(text) = resource.get("text").and_then(Value::as_str) {
                    texts.push(text.to_string());
                } else if let Some(data) = resource.get("blob").and_then(Value::as_str)
                    && let Some(path) = save_blob(blob_dir, index, mime, data, &mut notes)
                {
                    saved.push(path);
                }
            }
            "resource_link" => {
                if let Some(uri) = block.get("uri").and_then(Value::as_str) {
                    texts.push(uri.to_string());
                }
            }
            _ => {}
        }
    }
    let structured = result
        .get("structuredContent")
        .filter(|value| !value.is_null());
    let mut stdout = String::new();
    let mut stderr = String::new();
    if is_error {
        stderr = texts.join("\n");
        for path in &saved {
            if !stderr.is_empty() {
                stderr.push('\n');
            }
            stderr.push_str(path);
        }
    } else if let Some(structured) = structured {
        stdout = structured.to_string();
        // The paths must not corrupt JSON that a pipe is about to parse.
        stderr = saved.join("\n");
    } else {
        stdout = texts.join("\n");
        for path in &saved {
            if !stdout.is_empty() {
                stdout.push('\n');
            }
            stdout.push_str(path);
        }
    }
    for note in notes {
        if !stderr.is_empty() {
            stderr.push('\n');
        }
        stderr.push_str(&note);
    }
    Rendered {
        stdout,
        stderr,
        is_error,
        saved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dir(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("kelir-render-{name}-{}", ulid::Ulid::new()))
    }

    #[test]
    fn base64_decodes_both_alphabets_and_padding() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVs\nbG8").unwrap(), b"hello");
        assert_eq!(base64_decode("+/8=").unwrap(), [0xfb, 0xff]);
        assert_eq!(base64_decode("-_8=").unwrap(), [0xfb, 0xff]);
        assert_eq!(base64_decode("").unwrap(), b"");
        assert!(base64_decode("a$b").is_none());
    }

    #[test]
    fn text_blocks_are_joined_with_newlines() {
        let result = json!({"content": [
            {"type": "text", "text": "one"}, {"type": "text", "text": "two"}]});
        let out = render(&result, &dir("t"), false);
        assert_eq!(out.stdout, "one\ntwo");
        assert_eq!(out.exit_code(), EXIT_OK);
        assert!(out.stderr.is_empty());
    }

    #[test]
    fn structured_content_wins_and_is_compact_json() {
        let result = json!({
            "content": [{"type": "text", "text": "{\"a\": 1}"}],
            "structuredContent": {"a": 1, "b": [true]}});
        let out = render(&result, &dir("s"), false);
        assert_eq!(out.stdout, "{\"a\":1,\"b\":[true]}");
    }

    #[test]
    fn an_image_goes_to_a_file_and_its_path_is_printed() {
        let blob_dir = dir("i");
        let result = json!({"content": [
            {"type": "text", "text": "here"},
            {"type": "image", "mimeType": "image/png", "data": "aGVsbG8="}]});
        let out = render(&result, &blob_dir, false);
        assert_eq!(out.saved.len(), 1);
        assert!(out.saved[0].ends_with("1.png"), "{:?}", out.saved);
        assert_eq!(out.stdout, format!("here\n{}", out.saved[0]));
        assert_eq!(std::fs::read(&out.saved[0]).unwrap(), b"hello");
        let _ = std::fs::remove_dir_all(&blob_dir);
    }

    #[test]
    fn with_structured_json_the_paths_go_to_stderr() {
        let blob_dir = dir("b");
        let result = json!({
            "structuredContent": {"ok": true},
            "content": [{"type": "resource",
                "resource": {"uri": "x://y", "mimeType": "application/pdf", "blob": "aGk="}}]});
        let out = render(&result, &blob_dir, false);
        assert_eq!(out.stdout, "{\"ok\":true}");
        assert!(out.stderr.ends_with("0.pdf"), "{}", out.stderr);
        let _ = std::fs::remove_dir_all(&blob_dir);
    }

    #[test]
    fn an_error_result_prints_on_stderr_and_exits_1() {
        let result = json!({"isError": true, "content": [{"type": "text", "text": "nope"}]});
        let out = render(&result, &dir("e"), false);
        assert!(out.stdout.is_empty());
        assert_eq!(out.stderr, "nope");
        assert_eq!(out.exit_code(), EXIT_TOOL_ERROR);
    }

    #[test]
    fn the_envelope_is_the_whole_result_and_keeps_the_exit_code() {
        let result = json!({"isError": true, "content": [{"type": "text", "text": "x"}]});
        let out = render(&result, &dir("v"), true);
        assert_eq!(serde_json::from_str::<Value>(&out.stdout).unwrap(), result);
        assert_eq!(out.exit_code(), EXIT_TOOL_ERROR);
    }

    #[test]
    fn bad_base64_is_a_note_not_a_failure() {
        let result = json!({"content": [{"type": "image", "mimeType": "image/png", "data": "$$"}]});
        let out = render(&result, &dir("x"), false);
        assert!(out.saved.is_empty());
        assert!(out.stderr.contains("not valid base64"), "{}", out.stderr);
    }

    #[test]
    fn an_empty_result_prints_nothing() {
        let out = render(&json!({}), &dir("n"), false);
        assert!(out.stdout.is_empty() && out.stderr.is_empty());
    }
}
