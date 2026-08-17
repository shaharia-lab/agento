//! The `drive` service's three tools, ported from
//! `internal/integrations/google/drive.go`.
//!
//! Two things here are unlike anything in the other five integrations:
//!
//! 1. **`create_file` is a `multipart/related` upload** to a *different* path
//!    from every other Drive call — `/upload/drive/v3/files`, which the generated
//!    client resolves as an **absolute** reference and so replaces the base's
//!    whole path. Measured; see `client::resolve_relative`.
//! 2. **The media part's `Content-Type` is sniffed from the content**, not taken
//!    from the tool's `mime_type` argument, which reaches the metadata JSON
//!    alone. The sniff is `net/http.DetectContentType`, ported whole — and it is
//!    a *table*, not a text-versus-binary test, so a `content` beginning `BM`,
//!    `%PDF-`, `GIF89a` or `ID3` uploads as a bitmap, a PDF, a GIF or an MP3
//!    however innocent the prose that follows. See [`detect_content_type`], whose
//!    header explains why the earlier "only the reachable subset" version of it
//!    was wrong.
//!
//! And one shared with Gmail: `download_file` is the only call in the whole
//! integration that is not `alt=json` — it is `alt=media`, and its result is the
//! response body verbatim rather than a formatted summary.

use schemars::JsonSchema;
use serde_json::json;

use crate::claude::{new_tool, CancellationToken, ToolDef};
use crate::native::gourl::Values;

use super::client::{Api, Client, Multipart};
use super::text_result;

/// `list_files`.
#[allow(dead_code)] // read through serde, never constructed in Rust
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListFilesInput {
    /// Optional Drive query string (e.g. "name contains 'report'")
    query: String,
    /// Maximum number of files to return (default 10, max 100)
    max_results: i64,
}

pub fn list_files(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "list_files",
        "Lists files and folders in Google Drive.",
        move |input: ListFilesInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let max_results = if input.max_results <= 0 || input.max_results > 100 {
                    10
                } else {
                    input.max_results
                };

                let mut query = super::calendar::base_query();
                query.set(
                    "fields",
                    "files(id,name,mimeType,size,modifiedTime,webViewLink)",
                );
                query.set("pageSize", max_results.to_string());
                // Conditional, unlike Gmail's `q` — an empty Drive query sends
                // no key at all.
                if !input.query.is_empty() {
                    query.set("q", &input.query);
                }

                let listed: FileList = client
                    .get(&ct, Api::Drive, "files", &query)
                    .await
                    .and_then(|raw| super::decode(&raw))
                    .map_err(|e| format!("listing drive files: {e}"))?;
                if listed.files.is_empty() {
                    return Ok(text_result("No files found.".to_string()));
                }

                let rows: Vec<String> = listed
                    .files()
                    .map(|file| {
                        format!(
                            "Name: {}\nID: {}\nType: {}\nModified: {}\nLink: {}",
                            file.name,
                            file.id,
                            file.mime_type,
                            file.modified_time,
                            file.web_view_link
                        )
                    })
                    .collect();

                Ok(text_result(format!(
                    "Found {} file(s):\n\n{}",
                    listed.files.len(),
                    rows.join("\n\n---\n\n")
                )))
            }
        },
    )
}

/// `create_file`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateFileInput {
    /// required,Name of the file to create
    name: String,
    /// required,Text content of the file
    content: String,
    /// MIME type (default: text/plain)
    mime_type: String,
}

pub fn create_file(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "create_file",
        "Creates a new file in Google Drive with the provided content.",
        move |input: CreateFileInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                let mime_type = if input.mime_type.is_empty() {
                    "text/plain"
                } else {
                    &input.mime_type
                };

                let mut query = super::calendar::base_query();
                query.set("fields", "id,name,webViewLink");
                query.set("uploadType", "multipart");

                // The metadata part. `drive.File`'s fields carry `omitempty`, so
                // an empty `name` sends no key — `mimeType` always appears
                // because the handler defaults it above. Sorted, as
                // `json.Marshal` sorts a struct's declared order into the
                // generated field order (`mimeType` before `name`).
                let mut file = json!({"mimeType": mime_type});
                if !input.name.is_empty() {
                    file["name"] = serde_json::Value::String(input.name.clone());
                }
                let metadata = super::marshal(&file)?;

                // **Sniffed**, not `mime_type` — see the module docs.
                let media_type = detect_content_type(input.content.as_bytes());

                let created: File = client
                    .post_multipart(
                        &ct,
                        Api::Drive,
                        // Absolute, so it replaces the base's path entirely.
                        "/upload/drive/v3/files",
                        &query,
                        Multipart {
                            metadata,
                            media_type,
                            media: input.content.clone().into_bytes(),
                        },
                    )
                    .await
                    .and_then(|raw| super::decode(&raw))
                    .map_err(|e| format!("creating drive file: {e}"))?;
                Ok(text_result(format!(
                    "File created: {}\nID: {}\nLink: {}",
                    created.name, created.id, created.web_view_link
                )))
            }
        },
    )
}

/// `download_file`.
#[allow(dead_code)]
#[derive(serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DownloadFileInput {
    /// required,The Google Drive file ID to download
    file_id: String,
}

pub fn download_file(client: &Client) -> ToolDef {
    let client = client.clone();
    new_tool(
        "download_file",
        "Downloads and returns the text content of a Google Drive file by its ID.",
        move |input: DownloadFileInput, ct: CancellationToken| {
            let client = client.clone();
            async move {
                // The only call in the integration that is not `alt=json`.
                let mut query = Values::new();
                query.set("alt", "media");
                query.set("prettyPrint", "false");
                let path = format!(
                    "files/{}",
                    super::client::expand_path_segment(&input.file_id)
                );

                let body = client
                    .get(&ct, Api::Drive, &path, &query)
                    .await
                    // Go quotes the id with `%q`.
                    .map_err(|e| format!("downloading drive file {:?}: {e}", input.file_id))?;

                // The bytes, verbatim — no summary and no label.
                Ok(text_result(body))
            }
        },
    )
}

/// `drive.File`, reduced to the fields the handlers read.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct File {
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    id: String,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    name: String,
    #[serde(rename = "mimeType")]
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    mime_type: String,
    #[serde(rename = "modifiedTime")]
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    modified_time: String,
    #[serde(rename = "webViewLink")]
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    web_view_link: String,
}

#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct FileList {
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    files: Vec<crate::native::gojson::GoStruct<File>>,
}

impl FileList {
    fn files(&self) -> impl Iterator<Item = &File> {
        self.files.iter().map(|wrapped| &wrapped.0)
    }
}

/// `net/http.DetectContentType`, ported whole.
///
/// `googleapi`'s media writer sniffs the reader when no content type is set, so
/// this decides the media part's `Content-Type` and the tool's `mime_type`
/// argument does not.
///
/// # Why the whole table, when `content` is a JSON string
///
/// This was first written as "the subset a text argument can reach", on the
/// reasoning that a PNG's leading `0x89` cannot arrive as one byte because a Rust
/// `String` would UTF-8 encode it. That reasoning is sound for `0x89` and wrong
/// for most of the table, which review caught: `BM`, `%PDF-`, `%!PS-Adobe-`,
/// `GIF87a`/`GIF89a`, `ID3`, `OTTO`, `ttcf`, `wOFF`, `wOF2`, `RIFF…WAVE`,
/// `FORM…AIFF` and `OggS` are **pure ASCII**, and `PK\x03\x04`, `MThd`, the icon
/// signatures and the 34-NUL EOT one are all reachable through a JSON `\u0000`
/// escape. `BM` is the one that matters in practice: it is two bytes, so any
/// content beginning "BMI calculator results…" uploads as `image/bmp` — which is
/// what Go does, and is now what this does.
///
/// The lesson is the reason this is a transcription rather than a subset: an
/// argument about what is unreachable has to be right about *every* entry, and
/// it was not. Every signature is pinned by `desktop/parity/google_vectors.json`
/// or by the table test below.
///
/// The 512-byte truncation is part of the algorithm, not an optimisation — a
/// control byte past that offset does not make the upload binary.
fn detect_content_type(data: &[u8]) -> String {
    // `sniffLen`. `gax.DetermineContentType` also feeds the writer only the first
    // 512 bytes, so this bound applies twice in Go and once here.
    let data = &data[..data.len().min(512)];

    let first_non_ws = data
        .iter()
        .position(|b| !matches!(b, b'\t' | b'\n' | 0x0C | b'\r' | b' '))
        .unwrap_or(data.len());

    for sig in SNIFF_SIGNATURES {
        if let Some(ct) = sig.matches(data, first_non_ws) {
            return ct.to_string();
        }
    }
    "application/octet-stream".to_string()
}

/// One entry of Go's `sniffSignatures`, in its four flavours.
enum Sniff {
    /// `htmlSig` — case-insensitive on ASCII letters, and the next byte must
    /// terminate the tag, so `<html>` is HTML and `<htmlish` is not.
    Html(&'static [u8]),
    /// `exactSig` — a plain prefix.
    Exact(&'static [u8], &'static str),
    /// `maskedSig` — each byte compared after `&`ing with the mask.
    Masked(&'static [u8], &'static [u8], bool, &'static str),
    /// `mp4Sig` — a box-length walk rather than a prefix.
    Mp4,
    /// `textSig` — no byte in the control ranges. Always last.
    Text,
}

impl Sniff {
    fn matches(&self, data: &[u8], first_non_ws: usize) -> Option<&'static str> {
        match self {
            Self::Html(tag) => {
                let data = &data[first_non_ws..];
                if data.len() < tag.len() + 1 {
                    return None;
                }
                for (index, byte) in tag.iter().enumerate() {
                    let mut got = data[index];
                    if byte.is_ascii_uppercase() {
                        got &= 0xDF;
                    }
                    if *byte != got {
                        return None;
                    }
                }
                // 0xTT — a tag-terminating byte.
                matches!(data[tag.len()], b' ' | b'>').then_some("text/html; charset=utf-8")
            }
            Self::Exact(sig, ct) => data.starts_with(sig).then_some(*ct),
            Self::Masked(pat, mask, skip_ws, ct) => {
                let data = if *skip_ws {
                    &data[first_non_ws..]
                } else {
                    data
                };
                if pat.len() != mask.len() || data.len() < pat.len() {
                    return None;
                }
                pat.iter()
                    .zip(mask.iter())
                    .enumerate()
                    .all(|(index, (byte, mask))| data[index] & mask == *byte)
                    .then_some(*ct)
            }
            Self::Mp4 => {
                if data.len() < 12 {
                    return None;
                }
                let box_size = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
                if data.len() < box_size || !box_size.is_multiple_of(4) || &data[4..8] != b"ftyp" {
                    return None;
                }
                let mut start = 8;
                while start < box_size {
                    // The four bytes of the major brand's version are skipped.
                    if start != 12 && data.get(start..start + 3) == Some(b"mp4") {
                        return Some("video/mp4");
                    }
                    start += 4;
                }
                None
            }
            Self::Text => data[first_non_ws..]
                .iter()
                .all(|b| !matches!(b, 0x00..=0x08 | 0x0B | 0x0E..=0x1A | 0x1C..=0x1F))
                .then_some("text/plain; charset=utf-8"),
        }
    }
}

/// `net/http`'s `sniffSignatures`, **in order** — the order is the algorithm, not
/// a presentation choice, which is why the BOMs sit after the HTML tags and
/// `%PDF-` rather than "winning over everything" as an earlier comment here
/// claimed.
const SNIFF_SIGNATURES: &[Sniff] = &[
    Sniff::Html(b"<!DOCTYPE HTML"),
    Sniff::Html(b"<HTML"),
    Sniff::Html(b"<HEAD"),
    Sniff::Html(b"<SCRIPT"),
    Sniff::Html(b"<IFRAME"),
    Sniff::Html(b"<H1"),
    Sniff::Html(b"<DIV"),
    Sniff::Html(b"<FONT"),
    Sniff::Html(b"<TABLE"),
    Sniff::Html(b"<A"),
    Sniff::Html(b"<STYLE"),
    Sniff::Html(b"<TITLE"),
    Sniff::Html(b"<B"),
    Sniff::Html(b"<BODY"),
    Sniff::Html(b"<BR"),
    Sniff::Html(b"<P"),
    Sniff::Html(b"<!--"),
    Sniff::Masked(b"<?xml", b"\xFF\xFF\xFF\xFF\xFF", true, "text/xml; charset=utf-8"),
    Sniff::Exact(b"%PDF-", "application/pdf"),
    Sniff::Exact(b"%!PS-Adobe-", "application/postscript"),
    // UTF BOMs.
    Sniff::Masked(b"\xFE\xFF\x00\x00", b"\xFF\xFF\x00\x00", false, "text/plain; charset=utf-16be"),
    Sniff::Masked(b"\xFF\xFE\x00\x00", b"\xFF\xFF\x00\x00", false, "text/plain; charset=utf-16le"),
    Sniff::Masked(b"\xEF\xBB\xBF\x00", b"\xFF\xFF\xFF\x00", false, "text/plain; charset=utf-8"),
    // Images.
    Sniff::Exact(b"\x00\x00\x01\x00", "image/x-icon"),
    Sniff::Exact(b"\x00\x00\x02\x00", "image/x-icon"),
    Sniff::Exact(b"BM", "image/bmp"),
    Sniff::Exact(b"GIF87a", "image/gif"),
    Sniff::Exact(b"GIF89a", "image/gif"),
    Sniff::Masked(
        b"RIFF\x00\x00\x00\x00WEBPVP",
        b"\xFF\xFF\xFF\xFF\x00\x00\x00\x00\xFF\xFF\xFF\xFF\xFF\xFF",
        false,
        "image/webp",
    ),
    Sniff::Exact(b"\x89PNG\x0D\x0A\x1A\x0A", "image/png"),
    Sniff::Exact(b"\xFF\xD8\xFF", "image/jpeg"),
    // Audio and video, in the order the spec prescribes.
    Sniff::Masked(
        b"FORM\x00\x00\x00\x00AIFF",
        b"\xFF\xFF\xFF\xFF\x00\x00\x00\x00\xFF\xFF\xFF\xFF",
        false,
        "audio/aiff",
    ),
    Sniff::Masked(b"ID3", b"\xFF\xFF\xFF", false, "audio/mpeg"),
    Sniff::Masked(b"OggS\x00", b"\xFF\xFF\xFF\xFF\xFF", false, "application/ogg"),
    Sniff::Masked(
        b"MThd\x00\x00\x00\x06",
        b"\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF",
        false,
        "audio/midi",
    ),
    Sniff::Masked(
        b"RIFF\x00\x00\x00\x00AVI ",
        b"\xFF\xFF\xFF\xFF\x00\x00\x00\x00\xFF\xFF\xFF\xFF",
        false,
        "video/avi",
    ),
    Sniff::Masked(
        b"RIFF\x00\x00\x00\x00WAVE",
        b"\xFF\xFF\xFF\xFF\x00\x00\x00\x00\xFF\xFF\xFF\xFF",
        false,
        "audio/wave",
    ),
    Sniff::Mp4,
    Sniff::Exact(b"\x1A\x45\xDF\xA3", "video/webm"),
    // Fonts. The first is 34 NUL bytes followed by "LP".
    Sniff::Masked(
        b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00LP",
        b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\xFF\xFF",
        false,
        "application/vnd.ms-fontobject",
    ),
    Sniff::Exact(b"\x00\x01\x00\x00", "font/ttf"),
    Sniff::Exact(b"OTTO", "font/otf"),
    Sniff::Exact(b"ttcf", "font/collection"),
    Sniff::Exact(b"wOFF", "font/woff"),
    Sniff::Exact(b"wOF2", "font/woff2"),
    // Archives.
    Sniff::Exact(b"\x1F\x8B\x08", "application/x-gzip"),
    Sniff::Exact(b"PK\x03\x04", "application/zip"),
    Sniff::Exact(b"Rar!\x1A\x07\x00", "application/x-rar-compressed"),
    Sniff::Exact(b"Rar!\x1A\x07\x01\x00", "application/x-rar-compressed"),
    Sniff::Exact(b"\x00\x61\x73\x6D", "application/wasm"),
    Sniff::Text,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The signatures a `create_file` **argument** can actually reach, which is
    /// most of the table.
    ///
    /// The ASCII ones and the 512-byte bound are pinned in
    /// `desktop/parity/google_vectors.json` against the real
    /// `DetectContentType`; the rest are here because a vector for each would
    /// be a large fixture for a rule the transcription already states.
    ///
    /// This test replaced one that asserted only nine cases and the claim that
    /// "a binary signature cannot arrive as one byte" — the claim was false for
    /// every ASCII signature below, and the nine cases were exactly the ones
    /// that claim did not cover.
    #[test]
    fn the_media_type_is_sniffed_and_not_the_mime_type_argument() {
        for (content, want) in [
            // Ordinary prose, JSON and CSV.
            ("hello", "text/plain; charset=utf-8"),
            (r#"{"a":1}"#, "text/plain; charset=utf-8"),
            ("a,b\n1,2", "text/plain; charset=utf-8"),
            ("", "text/plain; charset=utf-8"),
            // The HTML table, its whitespace skip and its terminator rule.
            ("<html><body>hi</body></html>", "text/html; charset=utf-8"),
            ("  \n <HTML>", "text/html; charset=utf-8"),
            ("<!-- a comment", "text/html; charset=utf-8"),
            ("<htmlish thing", "text/plain; charset=utf-8"),
            // `<A` needs a terminator too, so a bare `<A` at end of input is not
            // HTML — the `len < tag.len() + 1` guard.
            ("<A", "text/plain; charset=utf-8"),
            ("<?xml version=\"1.0\"?>", "text/xml; charset=utf-8"),
            // Pure-ASCII signatures — all reachable from a JSON string, which is
            // the class the earlier version of this function missed entirely.
            ("BMI calculator results", "image/bmp"),
            ("%PDF-1.4 not really", "application/pdf"),
            ("%!PS-Adobe-3.0", "application/postscript"),
            ("GIF87a", "image/gif"),
            ("GIF89a still text", "image/gif"),
            ("ID3 v2 notes", "audio/mpeg"),
            ("OTTO font", "font/otf"),
            ("ttcf collection", "font/collection"),
            ("wOFF web font", "font/woff"),
            ("wOF2 web font", "font/woff2"),
            ("RIFF1234WAVEmore", "audio/wave"),
            ("RIFF1234AVI more", "video/avi"),
            ("FORM1234AIFFmore", "audio/aiff"),
            // Reachable through a JSON   escape.
            ("OggS\u{0}", "application/ogg"),
            ("MThd\u{0}\u{0}\u{0}\u{6}", "audio/midi"),
            ("PK\u{3}\u{4}", "application/zip"),
            ("\u{0}\u{0}\u{1}\u{0}", "image/x-icon"),
            ("\u{0}\u{1}\u{0}\u{0}", "font/ttf"),
            ("\u{0}asm", "application/wasm"),
            ("Rar!\u{1a}\u{7}\u{0}", "application/x-rar-compressed"),
            // The masked WEBP signature: the four length bytes are wildcards.
            ("RIFF????WEBPVP8 ", "image/webp"),
            // The text fallback and its control-byte set.
            ("a\u{0}b", "application/octet-stream"),
            ("a\u{b}b", "application/octet-stream"),
            // 0x09/0x0A/0x0C/0x0D and 0x1B are *not* binary.
            ("a\tb\nc\u{c}d\re\u{1b}f", "text/plain; charset=utf-8"),
        ] {
            assert_eq!(detect_content_type(content.as_bytes()), want, "{content:?}");
        }
    }

    /// `sniffLen`: the algorithm reads at most 512 bytes, so a control byte past
    /// that offset does not make the upload binary — and a signature past it is
    /// not seen either.
    #[test]
    fn only_the_first_512_bytes_are_sniffed() {
        let mut past = "a".repeat(600);
        past.push('\u{0}');
        assert_eq!(
            detect_content_type(past.as_bytes()),
            "text/plain; charset=utf-8"
        );

        let mut within = "a".repeat(10);
        within.push('\u{0}');
        assert_eq!(
            detect_content_type(within.as_bytes()),
            "application/octet-stream"
        );

        // Exactly at the boundary: index 511 is the last byte read.
        let mut edge = "a".repeat(511);
        edge.push('\u{0}');
        assert_eq!(
            detect_content_type(edge.as_bytes()),
            "application/octet-stream"
        );
        let mut just_past = "a".repeat(512);
        just_past.push('\u{0}');
        assert_eq!(
            detect_content_type(just_past.as_bytes()),
            "text/plain; charset=utf-8"
        );
    }

    /// The mp4 box walk, which is the one signature that is not a prefix test.
    #[test]
    fn the_mp4_signature_walks_boxes() {
        // A 20-byte box: length, "ftyp", a major brand, its version, then a
        // compatible brand of "mp4".
        let mut data = Vec::from(20u32.to_be_bytes());
        data.extend_from_slice(b"ftypisom");
        data.extend_from_slice(b"\x00\x00\x02\x00");
        data.extend_from_slice(b"mp41");
        assert_eq!(detect_content_type(&data), "video/mp4");

        // A box length that is not a multiple of four is rejected.
        let mut odd = Vec::from(21u32.to_be_bytes());
        odd.extend_from_slice(b"ftypisom....mp41");
        assert_ne!(detect_content_type(&odd), "video/mp4");
    }
}
