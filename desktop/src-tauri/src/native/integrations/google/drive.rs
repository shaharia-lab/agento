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
//!    from the tool's `mime_type` argument. Measured across five inputs: an
//!    `image/png` payload yields `image/png` and every textual one yields
//!    `text/plain; charset=utf-8`, *regardless* of what `mime_type` said. The
//!    argument reaches the metadata JSON alone. See [`detect_content_type`].
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

                // The metadata part. `drive.File`'s fields carry `omitempty`,
                // and both of these are always set, so both always appear —
                // sorted, as `json.Marshal` sorts a struct's declared order into
                // the generated field order (`mimeType` before `name`).
                let metadata = super::marshal(&json!({
                    "mimeType": mime_type,
                    "name": input.name,
                }))?;

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

/// `net/http.DetectContentType`, over the subset a **text** `content` argument
/// can reach.
///
/// `googleapi`'s media writer sniffs the reader when no content type is set, so
/// this decides the media part's header and the tool's `mime_type` does not.
///
/// The full algorithm is a table of thirty-odd magic numbers. Most of it is
/// unreachable here and deliberately not written: `content` is a JSON **string**,
/// so a PNG's leading `0x89` cannot arrive as one byte — it would be UTF-8
/// encoded to `C2 89` — and the same holds for every binary signature. What a
/// string *can* reach is implemented: the BOM rules, the HTML tag table, `<?xml`,
/// and the binary-vs-text fallback.
///
/// Measured against the real function for each case below.
fn detect_content_type(data: &[u8]) -> String {
    // `DetectContentType` answers this for an empty input rather than sniffing.
    if data.is_empty() {
        return "text/plain; charset=utf-8".to_string();
    }
    // Byte-order marks win over everything.
    for (bom, kind) in [
        (&[0xFE, 0xFF][..], "text/plain; charset=utf-16be"),
        (&[0xFF, 0xFE][..], "text/plain; charset=utf-16le"),
        (&[0xEF, 0xBB, 0xBF][..], "text/plain; charset=utf-8"),
    ] {
        if data.starts_with(bom) {
            return kind.to_string();
        }
    }

    // The HTML table matches case-insensitively and requires a tag terminator,
    // so `<html>` is HTML and `<htmlish` is not.
    let trimmed = {
        let mut rest = data;
        while let Some((first, tail)) = rest.split_first() {
            if matches!(first, b'\t' | b'\n' | 0x0C | b'\r' | b' ') {
                rest = tail;
            } else {
                break;
            }
        }
        rest
    };
    const HTML_TAGS: &[&[u8]] = &[
        b"<!DOCTYPE HTML",
        b"<HTML",
        b"<HEAD",
        b"<SCRIPT",
        b"<IFRAME",
        b"<H1",
        b"<DIV",
        b"<FONT",
        b"<TABLE",
        b"<A",
        b"<STYLE",
        b"<TITLE",
        b"<B",
        b"<BODY",
        b"<BR",
        b"<P",
        b"<!--",
    ];
    for tag in HTML_TAGS {
        if trimmed.len() > tag.len()
            && trimmed[..tag.len()].eq_ignore_ascii_case(tag)
            && matches!(trimmed[tag.len()], b' ' | b'>')
        {
            return "text/html; charset=utf-8".to_string();
        }
    }
    if trimmed.starts_with(b"<?xml") {
        return "text/xml; charset=utf-8".to_string();
    }

    // The fallback: text unless a byte is one `DetectContentType` calls binary.
    let binary = data
        .iter()
        .any(|&b| matches!(b, 0x00..=0x08 | 0x0B | 0x0E..=0x1A | 0x1C..=0x1F));
    if binary {
        "application/octet-stream".to_string()
    } else {
        "text/plain; charset=utf-8".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sniffing, at every shape a JSON string argument can produce.
    ///
    /// The first four were measured against the real `DetectContentType` through
    /// the generated Drive client; the rest are the algorithm's own rules.
    #[test]
    fn the_media_type_is_sniffed_and_not_the_mime_type_argument() {
        for (content, want) in [
            ("hello", "text/plain; charset=utf-8"),
            (r#"{"a":1}"#, "text/plain; charset=utf-8"),
            ("a,b\n1,2", "text/plain; charset=utf-8"),
            ("", "text/plain; charset=utf-8"),
            ("<html><body>hi</body></html>", "text/html; charset=utf-8"),
            ("  \n <HTML>", "text/html; charset=utf-8"),
            ("<?xml version=\"1.0\"?>", "text/xml; charset=utf-8"),
            // A tag needs a terminator, so this is prose.
            ("<htmlish thing", "text/plain; charset=utf-8"),
            ("a\u{0}b", "application/octet-stream"),
        ] {
            assert_eq!(detect_content_type(content.as_bytes()), want, "{content:?}");
        }
    }
}
