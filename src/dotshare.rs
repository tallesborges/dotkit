//! Dotshare interop: wrap a file in the Dotshare drop envelope so a plain
//! Bulletin CID opens as a *named, typed* drop in the Dotshare viewer instead of
//! falling back to raw byte sniffing (`drop-abc123.bin` / download).
//!
//! The v2 wire layout is frozen upstream (`dotshare/src/content.ts`); every field
//! below mirrors it byte for byte. Little-endian:
//!
//! ```text
//!   0   8   magic   = 89 44 53 48 52 0D 0A 1A   ("‰DSHR\r\n\x1a")
//!   8   1   version = 0x02
//!   9   4   metaLen = u32 LE, byte length of the meta JSON
//!   13  M   meta    = UTF-8 JSON { name, mime, enc }
//!   13+M .. data    = raw file bytes
//! ```
//!
//! Only unencrypted drops are produced here; password drops additionally need
//! Dotshare's HKDF-SHA256 + XChaCha20-Poly1305 packing, which dotkit doesn't
//! implement.

use serde_json::json;
use std::path::Path;

const MAGIC: [u8; 8] = [0x89, 0x44, 0x53, 0x48, 0x52, 0x0d, 0x0a, 0x1a];
const VERSION: u8 = 2;

/// DotNS label the Dotshare viewer is deployed under.
const APP_LABEL: &str = "dotshare";

/// Wrap `data` in an unencrypted Dotshare v2 envelope.
pub fn wrap(name: &str, mime: &str, data: &[u8]) -> Vec<u8> {
    let meta = json!({ "name": name, "mime": mime, "enc": false }).to_string();
    let meta = meta.as_bytes();

    let mut out = Vec::with_capacity(MAGIC.len() + 1 + 4 + meta.len() + data.len());
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&(meta.len() as u32).to_le_bytes());
    out.extend_from_slice(meta);
    out.extend_from_slice(data);
    out
}

/// Shareable viewer link. The CID travels in the URL *fragment*: a `?cid=` query
/// is dropped on the gateway's DotNS resolve→serve hop, so the viewer would never
/// see it. `gateway` is the env's public web gateway domain (e.g. `paseo.li`).
pub fn share_url(gateway: &str, cid: &str) -> String {
    format!("https://{APP_LABEL}.{gateway}/#cid={cid}")
}

/// The bare DotNS form of the same link — what a Polkadot host (e.g. Polkadot
/// Desktop) resolves natively, without going through a web gateway. `tld` is the
/// env's DotNS TLD (`paseo` on paseo-next-v2).
pub fn host_share_url(tld: &str, cid: &str) -> String {
    format!("https://{APP_LABEL}.{tld}/#cid={cid}")
}

/// Extension → MIME map mirroring Dotshare's table, so the viewer picks an inline
/// render mode (image / video / audio / pdf / markdown / highlighted code) rather
/// than offering a download.
fn mime_for_extension(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "txt" | "ini" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "csv" => "text/csv",
        "xml" => "text/xml",
        "css" => "text/css",
        "js" | "jsx" | "mjs" | "cjs" => "text/javascript",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "ogv" => "video/ogg",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "oga" | "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "aac" => "audio/aac",
        "ts" | "tsx" => "text/typescript",
        "py" => "text/x-python",
        "rs" => "text/x-rust",
        "go" => "text/x-go",
        "java" => "text/x-java",
        "kt" => "text/x-kotlin",
        "swift" => "text/x-swift",
        "rb" => "text/x-ruby",
        "php" => "text/x-php",
        "c" | "h" => "text/x-c",
        "cpp" | "cc" | "hpp" => "text/x-c++",
        "cs" => "text/x-csharp",
        "sh" | "bash" => "text/x-sh",
        "yml" | "yaml" => "text/yaml",
        "toml" => "text/x-toml",
        "sql" => "text/x-sql",
        "lua" => "text/x-lua",
        "scss" => "text/x-scss",
        "less" => "text/x-less",
        _ => return None,
    })
}

/// Infer the MIME type from a path's extension, defaulting to
/// `application/octet-stream` (the viewer then offers a download).
pub fn infer_mime(path: &Path) -> &'static str {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .and_then(|e| mime_for_extension(&e))
        .unwrap_or("application/octet-stream")
}

/// The drop's display name: the file name, or a generic fallback for paths
/// without one.
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("drop")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors Dotshare's `parseBinaryEnvelope` so the frozen layout is asserted
    /// end to end (header offsets + meta JSON + raw trailing bytes).
    #[test]
    fn envelope_matches_frozen_v2_layout() {
        let data = [0u8, 1, 2, 250];
        let bytes = wrap("cat.png", "image/png", &data);

        assert_eq!(&bytes[..8], &MAGIC);
        assert_eq!(bytes[8], 2);
        let meta_len = u32::from_le_bytes(bytes[9..13].try_into().unwrap()) as usize;
        let meta: serde_json::Value =
            serde_json::from_slice(&bytes[13..13 + meta_len]).expect("meta is JSON");
        assert_eq!(
            meta,
            json!({ "name": "cat.png", "mime": "image/png", "enc": false })
        );
        assert_eq!(&bytes[13 + meta_len..], &data);
    }

    #[test]
    fn mime_inference_falls_back_to_octet_stream() {
        assert_eq!(infer_mime(Path::new("a/b/notes.MD")), "text/markdown");
        assert_eq!(infer_mime(Path::new("clip.mov")), "video/quicktime");
        assert_eq!(
            infer_mime(Path::new("mystery.zzz")),
            "application/octet-stream"
        );
        assert_eq!(infer_mime(Path::new("noext")), "application/octet-stream");
    }

    #[test]
    fn links_put_the_cid_in_the_fragment() {
        assert_eq!(
            share_url("paseo.li", "bafy123"),
            "https://dotshare.paseo.li/#cid=bafy123"
        );
        assert_eq!(
            host_share_url("paseo", "bafy123"),
            "https://dotshare.paseo/#cid=bafy123"
        );
    }
}
