//! Pasted images become a file in the session's state dir plus a *pointer* in
//! the prompt; the bytes never ride in a message (design note TUI-v2 §5).
//!
//! Why a pointer: image bytes in the prompt are paid for on **every** later
//! request (both opencode and Claude Code do that), and they would break the
//! byte-stable cache prefix. Instead the model decides whether to look, with
//! `kelir view <path>`, which appends a `source` event (kind `attachment`) at the
//! **tail** of the conversation. Every earlier message stays byte-identical.
//!
//! Three ingress paths, one result:
//!
//! 1. A pasted or drag-dropped **path** (`Event::Paste`): trim, unquote, accept
//!    the PowerShell `& '…'` form, a `file://` URL and backslash escapes. This
//!    is the one that works in every terminal, so it ships first.
//! 2. The **clipboard image** on Ctrl-V, where the terminal sends the key
//!    instead of a paste (a stub here: [`clipboard_image`]).
//! 3. A terminal's own image paste: no portable protocol, so it simply degrades
//!    into 1 or 2.
//!
//! Dimensions come from the file header by hand (PNG IHDR, JPEG SOF, GIF, WebP
//! VP8/VP8L/VP8X): no decoder crate, no allocation of the pixels.

use std::path::{Path, PathBuf};

/// What a paste that names an image file looks like after parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PastedImage {
    pub path: PathBuf,
}

/// The image extensions kelir treats as an attachment.
pub const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

/// The most a paste may weigh (and the most a clipboard image may).
pub const MAX_BYTES: u64 = 10 * 1024 * 1024;
/// How many images one prompt may carry.
pub const MAX_PER_PROMPT: usize = 8;

/// Strip the wrappers a terminal, a shell or PowerShell puts around a dropped
/// path, without touching a path that only looks quoted.
///
/// Handles: surrounding whitespace, one pair of `"…"` or `'…'`, the PowerShell
/// drag form `& 'C:\…'`, a `file://` URL (with percent escapes for the three
/// characters a Windows path uses: `%20`, `%23`, `%25`), and backslash-escaped
/// spaces (`\ `), which shells add.
pub fn clean(text: &str) -> String {
    let mut text = text.trim();
    // PowerShell's drag-and-drop prefix.
    if let Some(rest) = text.strip_prefix("& ") {
        text = rest.trim();
    }
    // One pair of matching quotes.
    for quote in ['"', '\''] {
        if text.len() >= 2 && text.starts_with(quote) && text.ends_with(quote) {
            text = &text[1..text.len() - 1];
            break;
        }
    }
    let mut text = text.trim().to_string();
    if let Some(rest) = text.strip_prefix("file://") {
        // `file:///C:/x` (an empty host) and `file://host/share/x` both keep
        // their slashes after the host.
        let rest = rest.strip_prefix("localhost/").unwrap_or(rest);
        // On Windows a URL path starts `/C:/…`; drop that first slash.
        let rest = match rest.strip_prefix('/') {
            Some(inner) if inner.as_bytes().get(1) == Some(&b':') => inner,
            _ => rest,
        };
        text = unescape_url(rest);
    }
    if text.contains("\\ ") {
        text = text.replace("\\ ", " ");
    }
    text.trim().to_string()
}

fn unescape_url(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let rest: String = chars.clone().take(2).collect();
        if let Ok(byte) = u8::from_str_radix(&rest, 16) {
            for _ in 0..2 {
                chars.next();
            }
            out.push(byte as char);
        } else {
            out.push(c);
        }
    }
    out
}

/// The file a paste names, when it is one image path and nothing else.
///
/// A paste that is not exactly this (a very long line of text, a sentence that
/// happens to end in `.png`, a path that does not exist) is text, and the
/// caller pastes it as text.
pub fn pasted_image(text: &str) -> Option<PastedImage> {
    let cleaned = clean(text);
    if cleaned.is_empty() {
        return None;
    }
    if cleaned.contains('\n') || cleaned.contains('\r') {
        return None;
    }
    let path = Path::new(&cleaned);
    if !extension_ok(path) || !path.is_file() {
        return None;
    }
    // The extension is a claim: a text file named `fake.png` is not an image,
    // and a chip that promises one is a chip the model cannot use (`N-chip`).
    // The bytes decide, the same check `kelir view` makes (`S3`).
    match sniff(path) {
        Ok(Some(_)) => {}
        _ => return None,
    }
    Some(PastedImage {
        path: path.to_path_buf(),
    })
}

/// The path-shaped tail of `text`: the run of characters a dropped or typed
/// path could be. A quoted run wins (`"C:\a b.png"`, `& 'C:\a.png'`) because it
/// can hold spaces; otherwise the last non-space token is the candidate.
///
/// Returns `None` when the tail cannot be an image path at all (empty, no image
/// extension in it, too long to be one). Nothing here touches the file system:
/// the caller asks [`pasted_image`], which is the one rule both ingress paths
/// (a bracketed paste and a keystroke burst) obey.
pub fn path_tail(text: &str) -> Option<String> {
    let text = text.trim_end();
    if text.is_empty() || text.chars().count() > MAX_PATH_CHARS {
        return None;
    }
    let last = text.chars().next_back()?;
    if last == '"' || last == '\'' {
        // A closed quote: the run starts at its opener.
        if let Some(open) = text[..text.len() - last.len_utf8()].rfind(last) {
            let quoted = &text[open..];
            return extension_ok_str(quoted).then(|| quoted.to_string());
        }
        return None;
    }
    let start = text
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map(|(index, c)| index + c.len_utf8())
        .unwrap_or(0);
    let word = &text[start..];
    // `& 'C:\a.png'` is PowerShell's drag form: the word is the quoted part.
    let word = word.strip_prefix("& ").unwrap_or(word);
    extension_ok_str(word).then(|| word.to_string())
}

/// The longest text [`path_tail`] considers: a path, not the paragraph someone
/// happened to be typing.
pub const MAX_PATH_CHARS: usize = 512;

/// Does this text end in an image extension? Cheap enough to run on every
/// candidate token before the file system is touched.
pub fn extension_ok_str(text: &str) -> bool {
    extension_ok(Path::new(text.trim_matches(['"', '\'', '&', ' '])))
}

fn extension_ok(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
        .unwrap_or(false)
}

/// The MIME type of a saved attachment.
///
/// The bytes decide it, never the file's name: an extension is a claim (a WebP
/// saved as `shot.png` is what browsers produce, a GIF renamed `.png` is a
/// rename), and a declared type that contradicts the bytes is what a provider
/// that checks either of them against the other rejects — a 400 that repeats on
/// every later request, because the `source` event stays in the log (`S3b`).
///
/// The extension is the fallback for a file that cannot be read, so a missing
/// attachment is still named by what it claimed rather than as an opaque blob.
pub fn mime_of(path: &Path) -> &'static str {
    match sniff(path) {
        Ok(Some((format, _))) => mime_of_format(format),
        _ => mime_from_extension(path),
    }
}

/// The media type of a format [`sniff`] names (`png`, `jpg`, `gif`, `webp`).
pub fn mime_of_format(format: &str) -> &'static str {
    match format {
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

/// The media type a file's extension claims: a fallback only, because the bytes
/// are the truth (`S3b`).
fn mime_from_extension(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    }
}

/// `attachments/<seq>-<name>.png`: the name inside the session dir. The source
/// name is sanitised to `[A-Za-z0-9._-]` and shortened, so a pasted path can
/// never escape the directory or blow the name budget.
pub fn file_name(seq: u32, source: &Path) -> String {
    let extension = source
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|extension| EXTENSIONS.contains(&extension.as_str()))
        .unwrap_or_else(|| "png".to_string());
    let stem = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(sanitise)
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "clipboard".to_string());
    format!("{seq}-{stem}.{extension}")
}

/// Every image path a message names, in the order they appear: what the `user`
/// event's `attachments[]` carries (the design note: the list holds the path
/// while the text holds the pointer).
///
/// A pointer is `[image: <path>, …]`, written only by [`pointer`], so the scan
/// is the inverse of that one function and cannot drift from it.
pub fn paths_in(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("[image: ") {
        let after = &rest[start + "[image: ".len()..];
        let Some(end) = after.find(']') else {
            break;
        };
        let inner = &after[..end];
        // The path is the first comma-separated field; older pointers carry it
        // alone.
        let path = inner.split(',').next().unwrap_or_default().trim();
        if !path.is_empty() && !out.iter().any(|seen| seen == path) {
            out.push(path.to_string());
        }
        rest = &after[end..];
    }
    out
}

/// The sequence number the session's next copy gets: one more than the highest
/// `<n>-…` already in `<session>/attachments/` (`1` for the first). A number is
/// a count and a label, not an identity: the file name only has to be unique,
/// which it is by suffix as well.
pub fn next_seq(session_dir: &Path) -> u32 {
    let dir = dir(session_dir);
    let highest = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            name.split('-').next()?.parse::<u32>().ok()
        })
        .max()
        .unwrap_or(0);
    highest.saturating_add(1)
}

fn sanitise(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .take(40)
        .collect()
}

/// `[image: attachments/1-clipboard.png, 1280x720, 312 KB]`: the pointer the
/// prompt carries instead of the bytes.
///
/// The pointer says how to look at what it names (`kelir view <path>`), because
/// the model otherwise has no way to learn that it can: the review's live run
/// spent seven steps guessing a picture that a pointer and a hint would have
/// given it in two (`S4`).
pub fn pointer(path_in_session: &str, size: Option<(u32, u32)>, bytes: u64) -> String {
    let mut parts = vec![path_in_session.to_string()];
    if let Some((width, height)) = size {
        parts.push(format!("{width}x{height}"));
    }
    parts.push(bytes_label(bytes));
    format!(
        "[image: {} — run: kelir view {path_in_session}]",
        parts.join(", ")
    )
}

/// The menu chip's label: `[image 1]`.
pub fn chip_label(seq: u32) -> String {
    format!("[image {seq}]")
}

pub fn bytes_label(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// A file's own claim about itself: the format its signature names and the size
/// its header states.
pub type Identity = (&'static str, (u32, u32));

/// What a file's first bytes say it is: `Ok(Some(identity))` for a format
/// kelir reads, `Ok(None)` when the bytes are not one (an empty file, a text
/// file with a `.png` name), `Err` when it cannot even be read.
///
/// The extension is a claim the file makes; this is the file's own signature.
/// An image is refused before it reaches the log because a bad one stays in the
/// conversation: every later request repeats it, and the provider's refusal
/// repeats with it (`S3`).
pub fn sniff(path: &Path) -> Result<Option<Identity>, String> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    // Headers live in the first few hundred bytes (a JPEG walks its segments up
    // to the SOF, so it gets a generous window); never read a whole image just
    // to identify it.
    let mut head = vec![0u8; SNIFF_BYTES];
    let mut filled = 0usize;
    while filled < head.len() {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
        }
    }
    head.truncate(filled);
    let format = if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        "png"
    } else if head.starts_with(&[0xff, 0xd8]) {
        "jpg"
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        "gif"
    } else if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
        "webp"
    } else {
        return Ok(None);
    };
    Ok(dimensions(&head).map(|size| (format, size)))
}

/// How much of a file [`sniff`] reads: enough for a JPEG's SOF segment, nowhere
/// near enough to cost anything for a 10 MB image.
pub const SNIFF_BYTES: usize = 64 * 1024;

/// The image's dimensions from its header, never decoding the pixels.
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    png(bytes)
        .or_else(|| jpeg(bytes))
        .or_else(|| gif(bytes))
        .or_else(|| webp(bytes))
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    let slice = bytes.get(at..at + 4)?;
    Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn be16(bytes: &[u8], at: usize) -> Option<u16> {
    let slice = bytes.get(at..at + 2)?;
    Some(u16::from_be_bytes([slice[0], slice[1]]))
}

fn le24(bytes: &[u8], at: usize) -> Option<u32> {
    let slice = bytes.get(at..at + 3)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], 0]))
}

fn png(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(..8)? != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    // IHDR is the first chunk: length(4) type(4) width(4) height(4).
    if bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    Some((be32(bytes, 16)?, be32(bytes, 20)?))
}

fn gif(bytes: &[u8]) -> Option<(u32, u32)> {
    let header = bytes.get(..6)?;
    if header != b"GIF87a" && header != b"GIF89a" {
        return None;
    }
    let width = be16(bytes, 6)? as u32;
    let height = be16(bytes, 8)? as u32;
    Some((width, height))
}

fn jpeg(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(..2)? != b"\xff\xd8" {
        return None;
    }
    let mut at = 2usize;
    while at + 3 < bytes.len() {
        if bytes[at] != 0xFF {
            at += 1;
            continue;
        }
        let marker = bytes[at + 1];
        // Standalone markers carry no length.
        if (0xD0..=0xD9).contains(&marker) || marker == 0x01 || marker == 0xFF {
            at += 2;
            continue;
        }
        let length = be16(bytes, at + 2)? as usize;
        // SOF0-SOF15 (except DHT/JPG/DAC, which are not frames).
        if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
            let height = be16(bytes, at + 5)? as u32;
            let width = be16(bytes, at + 7)? as u32;
            return Some((width, height));
        }
        at += 2 + length;
    }
    None
}

fn webp(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(..4)? != b"RIFF" || bytes.get(8..12)? != b"WEBP" {
        return None;
    }
    match bytes.get(12..16)? {
        b"VP8 " => {
            // The keyframe header: 3-byte frame tag, sync code, then 16-bit
            // width and height (14 bits each).
            if bytes.get(23..26)? != b"\x9d\x01\x2a" {
                return None;
            }
            let width = u16::from_le_bytes([bytes[26], bytes[27]]) as u32 & 0x3fff;
            let height = u16::from_le_bytes([bytes[28], bytes[29]]) as u32 & 0x3fff;
            Some((width, height))
        }
        b"VP8L" => {
            // 0x2f, then 14-bit (width-1), 14-bit (height-1).
            if bytes.get(20)? != &0x2f {
                return None;
            }
            let bits = u32::from_le_bytes([bytes[21], bytes[22], bytes[23], bytes[24]]);
            let width = (bits & 0x3fff) + 1;
            let height = ((bits >> 14) & 0x3fff) + 1;
            Some((width, height))
        }
        b"VP8X" => {
            let width = le24(bytes, 24)? + 1;
            let height = le24(bytes, 27)? + 1;
            Some((width, height))
        }
        _ => None,
    }
}

/// The directory a session's images live in: `<session>/attachments`.
pub fn dir(session_dir: &Path) -> PathBuf {
    session_dir.join("attachments")
}

/// One saved image: what the prompt points at, and what the chip stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    /// The per-prompt sequence number (1-based), also the chip's number.
    pub seq: u32,
    /// `attachments/1-clipboard.png`: relative to the session dir, which is
    /// what the prompt carries (never an absolute path: the model resolves it
    /// against the session dir, and the log stays free of host paths).
    pub rel: String,
    /// The full pointer text the prompt gets.
    pub pointer: String,
    pub bytes: u64,
    /// The bytes were already saved in this session (same content).
    pub deduped: bool,
}

/// Copy `source` into `<session>/attachments/` and describe the result.
///
/// The name is `[seq]-<sanitised stem>.<ext>`; a second paste of the same bytes
/// reuses the first file (a hash of the content, [`fingerprint`], is the key),
/// so a chatty paste does not fill the disk. Refuses anything over
/// [`MAX_BYTES`].
pub fn save(
    session_dir: &Path,
    seq: u32,
    source: &Path,
    seen: &mut std::collections::HashMap<u64, String>,
) -> Result<Saved, String> {
    let bytes = std::fs::read(source)
        .map_err(|error| format!("cannot read {}: {error}", source.display()))?;
    let size = bytes.len() as u64;
    if size > MAX_BYTES {
        return Err(format!(
            "{} is {} (the limit is {})",
            source.display(),
            bytes_label(size),
            bytes_label(MAX_BYTES)
        ));
    }
    let key = fingerprint(&bytes);
    if let Some(rel) = seen.get(&key) {
        let dimensions = dimensions(&bytes);
        return Ok(Saved {
            seq,
            rel: rel.clone(),
            pointer: pointer(rel, dimensions, size),
            bytes: size,
            deduped: true,
        });
    }
    let name = file_name(seq, source);
    let rel = format!("attachments/{name}");
    let dir = dir(session_dir);
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let target = dir.join(&name);
    std::fs::write(&target, &bytes)
        .map_err(|error| format!("cannot write {}: {error}", target.display()))?;
    seen.insert(key, rel.clone());
    Ok(Saved {
        seq,
        rel: rel.clone(),
        pointer: pointer(&rel, dimensions(&bytes), size),
        bytes: size,
        deduped: false,
    })
}

/// A 64-bit content fingerprint (FNV-1a): enough to notice the same image
/// pasted twice, and one dependency less than a real hash.
pub fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// `kelir view`'s path resolution: a path that starts with `attachments/`
/// resolves against the session dir first (that is what the prompt carries),
/// then the cwd; anything else is taken from the cwd.
pub fn resolve(session_dir: &Path, cwd: &Path, named: &str) -> PathBuf {
    let cleaned = clean(named);
    let path = Path::new(&cleaned);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    let in_session = session_dir.join(path);
    if cleaned.starts_with("attachments") && in_session.is_file() {
        return in_session;
    }
    let in_cwd = cwd.join(path);
    if in_cwd.is_file() {
        return in_cwd;
    }
    in_session
}

/// `data:image/png;base64,…`: the `image_url` an OpenAI-compatible request
/// carries.
pub fn data_url(mime: &str, bytes: &[u8]) -> String {
    format!("data:{mime};base64,{}", base64(bytes))
}

/// Standard base64 with padding, which is what the providers accept. Twenty
/// lines instead of a dependency (the binary budget is a feature).
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
        let word = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(word >> 18) as usize & 63] as char);
        out.push(ALPHABET[(word >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(word >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[word as usize & 63] as char
        } else {
            '='
        });
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes
    }

    fn gif_bytes(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[0, 0, 0]);
        bytes
    }

    fn jpeg_bytes(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = b"\xff\xd8".to_vec();
        // An APP0 segment first, so the scanner has to walk past it.
        bytes.extend_from_slice(b"\xff\xe0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00");
        // SOF0: length 17, precision 8, height, width, 3 components.
        bytes.extend_from_slice(b"\xff\xc0\x00\x11\x08");
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        bytes
    }

    fn webp_vp8x(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&30u32.to_le_bytes());
        bytes.extend_from_slice(b"WEBPVP8X");
        bytes.extend_from_slice(&10u32.to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        let w = (width - 1).to_le_bytes();
        let h = (height - 1).to_le_bytes();
        bytes.extend_from_slice(&w[..3]);
        bytes.extend_from_slice(&h[..3]);
        bytes
    }

    #[test]
    fn a_quoted_or_escaped_path_is_unwrapped() {
        assert_eq!(clean("  /tmp/a.png  "), "/tmp/a.png");
        assert_eq!(clean("\"/tmp/with space.png\""), "/tmp/with space.png");
        assert_eq!(clean("'/tmp/with space.png'"), "/tmp/with space.png");
        assert_eq!(
            clean("& 'C:\\Users\\x\\shot.png'"),
            "C:\\Users\\x\\shot.png"
        );
        assert_eq!(clean("/tmp/with\\ space.png"), "/tmp/with space.png");
        assert_eq!(clean("file:///tmp/a.png"), "/tmp/a.png");
        assert_eq!(clean("file:///C:/x/a%20b.png"), "C:/x/a b.png");
        // A quotation mark in the middle is not a wrapper.
        assert_eq!(clean("it's fine.png"), "it's fine.png");
    }

    #[test]
    fn only_a_single_existing_image_path_becomes_a_paste() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("shot.PNG");
        std::fs::write(&image, png_bytes(4, 3)).unwrap();
        let quoted = format!("\"{}\"", image.display());
        assert_eq!(
            pasted_image(&quoted),
            Some(PastedImage {
                path: image.clone()
            })
        );
        // A path that does not exist, a non-image extension and multi-line text
        // are all text.
        assert_eq!(pasted_image(&format!("{}.png", image.display())), None);
        let notes = dir.path().join("notes.txt");
        std::fs::write(&notes, "x").unwrap();
        assert_eq!(pasted_image(&notes.display().to_string()), None);
        assert_eq!(
            pasted_image(&format!("{}\n{}", image.display(), image.display())),
            None
        );
        assert_eq!(pasted_image(""), None);
        assert_eq!(
            pasted_image("this sentence ends in .png but is not a path"),
            None
        );
    }

    /// Review N5: the `user` event's `attachments[]` names the images the text
    /// points at, so a reader does not have to parse prose.
    #[test]
    fn a_pointer_is_the_inverse_of_the_path_list() {
        let one = pointer("attachments/1-shot.png", Some((4, 3)), 726);
        assert_eq!(
            paths_in(&format!("what is this? {one}")),
            vec!["attachments/1-shot.png".to_string()]
        );
        // Two images, one repeated: the list is ordered and de-duplicated.
        let two = format!(
            "{a} and {b} and again {a}",
            a = pointer("attachments/1-a.png", None, 10),
            b = pointer("attachments/2-b.jpg", None, 20)
        );
        assert_eq!(
            paths_in(&two),
            vec![
                "attachments/1-a.png".to_string(),
                "attachments/2-b.jpg".to_string()
            ]
        );
        // Text with no pointer, and a broken one, name nothing.
        assert!(paths_in("no images here").is_empty());
        assert!(paths_in("[image: attachments/1-a.png").is_empty());
    }

    /// Review N6: copies are numbered in the order they arrive, not all `1-…`.
    #[test]
    fn a_sessions_copies_are_numbered_in_sequence() {
        let session = tempfile::tempdir().unwrap();
        assert_eq!(next_seq(session.path()), 1, "a fresh session starts at 1");
        let attachments = dir(session.path());
        std::fs::create_dir_all(&attachments).unwrap();
        std::fs::write(attachments.join("1-a.png"), png_bytes(4, 3)).unwrap();
        std::fs::write(attachments.join("2-b.png"), png_bytes(4, 3)).unwrap();
        assert_eq!(next_seq(session.path()), 3);
        // A name that is not numbered does not confuse the count.
        std::fs::write(attachments.join("notes.png"), png_bytes(4, 3)).unwrap();
        assert_eq!(next_seq(session.path()), 3);
    }

    /// Review S3: the signature decides, not the extension.
    #[test]
    fn sniff_reads_the_signature_and_the_header() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("ok.png");
        std::fs::write(&good, png_bytes(4, 3)).unwrap();
        assert_eq!(sniff(&good).unwrap(), Some(("png", (4, 3))));
        let fake = dir.path().join("fake.png");
        std::fs::write(&fake, b"not a png").unwrap();
        assert_eq!(sniff(&fake).unwrap(), None);
        let empty = dir.path().join("empty.png");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(sniff(&empty).unwrap(), None);
        let jpeg = dir.path().join("x.jpg");
        std::fs::write(&jpeg, jpeg_bytes(800, 600)).unwrap();
        assert_eq!(sniff(&jpeg).unwrap(), Some(("jpg", (800, 600))));
        let gif = dir.path().join("x.gif");
        std::fs::write(&gif, gif_bytes(64, 32)).unwrap();
        assert_eq!(sniff(&gif).unwrap(), Some(("gif", (64, 32))));
        // A file that cannot be read is an error, not a verdict.
        assert!(sniff(&dir.path().join("missing.png")).is_err());
    }

    /// Review S4: the path tail a keystroke burst is matched against.
    #[test]
    fn a_path_tail_is_the_word_or_the_quoted_run_that_ends_the_text() {
        assert_eq!(
            path_tail("look at C:/tmp/a.png").as_deref(),
            Some("C:/tmp/a.png")
        );
        assert_eq!(
            path_tail("look at \"C:/tmp/a b.png\"").as_deref(),
            Some("\"C:/tmp/a b.png\"")
        );
        assert_eq!(
            path_tail("& 'C:/tmp/a.png'").as_deref(),
            Some("'C:/tmp/a.png'")
        );
        // Not a path: no image extension, or an open quote with nothing after it.
        assert_eq!(path_tail("look at C:/tmp/a.txt"), None);
        assert_eq!(path_tail("look at \""), None);
        assert_eq!(path_tail(""), None);
        assert!(path_tail(&"x".repeat(MAX_PATH_CHARS + 1)).is_none());
    }

    #[test]
    fn dimensions_come_from_the_header_of_every_supported_format() {
        assert_eq!(dimensions(&png_bytes(1280, 720)), Some((1280, 720)));
        assert_eq!(dimensions(&gif_bytes(64, 32)), Some((64, 32)));
        assert_eq!(dimensions(&jpeg_bytes(800, 600)), Some((800, 600)));
        assert_eq!(dimensions(&webp_vp8x(1024, 768)), Some((1024, 768)));
        // Truncated or foreign input is `None`, never a panic.
        assert_eq!(dimensions(b""), None);
        assert_eq!(dimensions(b"\x89PNG"), None);
        assert_eq!(dimensions(&png_bytes(1, 1)[..20]), None);
        assert_eq!(dimensions(b"not an image at all"), None);
    }

    #[test]
    fn the_saved_name_is_numbered_sanitised_and_never_escapes_the_directory() {
        let name = file_name(1, Path::new("C:/x/../../etc/shots/my shot.png"));
        assert!(name.starts_with("1-"), "{name}");
        assert!(name.ends_with(".png"), "{name}");
        assert!(!name.contains('/') && !name.contains('\\'), "{name}");
        // A file with no usable stem still gets a name.
        assert_eq!(file_name(2, Path::new("")), "2-clipboard.png");
        // An unknown extension is treated as a PNG rather than trusted.
        assert_eq!(file_name(3, Path::new("a.exe")), "3-a.png");
    }

    #[test]
    fn the_pointer_names_the_relative_path_the_size_and_the_dimensions() {
        assert_eq!(
            pointer("attachments/1-clipboard.png", Some((1280, 720)), 319_488),
            "[image: attachments/1-clipboard.png, 1280x720, 312 KB — run: kelir view attachments/1-clipboard.png]"
        );
        // No dimensions: the pointer still says where the file is.
        assert_eq!(
            pointer("attachments/2-x.webp", None, 900),
            "[image: attachments/2-x.webp, 900 B — run: kelir view attachments/2-x.webp]"
        );
        assert_eq!(chip_label(1), "[image 1]");
    }

    #[test]
    fn saving_copies_the_bytes_names_them_and_dedupes_the_same_content() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("shot.png");
        let bytes = png_bytes(1280, 720);
        std::fs::write(&source, &bytes).unwrap();
        let mut seen = std::collections::HashMap::new();
        let saved = save(dir.path(), 1, &source, &mut seen).unwrap();
        assert_eq!(saved.seq, 1);
        assert_eq!(saved.rel, "attachments/1-shot.png");
        assert!(!saved.deduped);
        assert_eq!(saved.bytes, bytes.len() as u64);
        assert_eq!(
            std::fs::read(dir.path().join(&saved.rel)).unwrap(),
            bytes,
            "the file is really there"
        );
        assert_eq!(
            saved.pointer,
            format!(
                "[image: attachments/1-shot.png, 1280x720, {} — run: kelir view attachments/1-shot.png]",
                bytes_label(bytes.len() as u64)
            )
        );
        // The same content pasted again reuses the first file.
        let again = save(dir.path(), 2, &source, &mut seen).unwrap();
        assert!(again.deduped);
        assert_eq!(again.rel, saved.rel);
        assert_eq!(again.seq, 2, "the chip still counts per prompt");
        // Different content gets its own file.
        let other = dir.path().join("other.png");
        std::fs::write(&other, png_bytes(8, 8)).unwrap();
        let third = save(dir.path(), 3, &other, &mut seen).unwrap();
        assert!(!third.deduped);
        assert_ne!(third.rel, saved.rel);
    }

    #[test]
    fn saving_refuses_a_missing_file_and_one_over_the_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut seen = std::collections::HashMap::new();
        let missing = save(dir.path(), 1, &dir.path().join("nope.png"), &mut seen);
        assert!(missing.is_err(), "{missing:?}");

        let big = dir.path().join("big.png");
        let mut bytes = png_bytes(10, 10);
        bytes.resize(MAX_BYTES as usize + 1, 0);
        std::fs::write(&big, &bytes).unwrap();
        let refused = save(dir.path(), 1, &big, &mut seen);
        let message = refused.unwrap_err();
        assert!(message.contains("limit"), "{message}");
    }

    #[test]
    fn view_resolves_an_attachment_against_the_session_then_the_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("session");
        let cwd = dir.path().join("cwd");
        std::fs::create_dir_all(session.join("attachments")).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let saved = session.join("attachments/1-x.png");
        std::fs::write(&saved, png_bytes(2, 2)).unwrap();
        assert_eq!(resolve(&session, &cwd, "attachments/1-x.png"), saved);
        let local = cwd.join("pic.png");
        std::fs::write(&local, png_bytes(2, 2)).unwrap();
        assert_eq!(resolve(&session, &cwd, "pic.png"), local);
        // An absolute path is taken as given.
        assert_eq!(resolve(&session, &cwd, &saved.display().to_string()), saved);
    }

    #[test]
    fn base64_matches_a_known_encoding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(data_url("image/png", b"foo"), "data:image/png;base64,Zm9v");
    }

    #[test]
    fn the_mime_type_follows_the_extension() {
        assert_eq!(mime_of(Path::new("a.png")), "image/png");
        assert_eq!(mime_of(Path::new("a.JPG")), "image/jpeg");
        assert_eq!(mime_of(Path::new("a.jpeg")), "image/jpeg");
        assert_eq!(mime_of(Path::new("a.gif")), "image/gif");
        assert_eq!(mime_of(Path::new("a.webp")), "image/webp");
        assert_eq!(mime_of(Path::new("a.bin")), "application/octet-stream");
    }

    #[test]
    fn a_fingerprint_notices_equal_and_different_content() {
        let a = png_bytes(4, 4);
        let b = png_bytes(4, 4);
        let c = png_bytes(4, 5);
        assert_eq!(fingerprint(&a), fingerprint(&b));
        assert_ne!(fingerprint(&a), fingerprint(&c));
        assert_ne!(fingerprint(b""), fingerprint(b"\0"));
    }
}
