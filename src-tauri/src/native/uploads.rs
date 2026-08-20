//! `POST /api/uploads`, ported from `internal/api/uploads.go`.
//!
//! The chat composer posts a file here and gets back an **absolute path**,
//! which it then injects into the prompt it sends the model. So the interesting
//! part of this route is not the JSON — it is one key — but the filename, and
//! `sanitize_extension` is the boundary between a name the user chose and a path
//! this process creates.
//!
//! # The one multipart body in the API
//!
//! Every other claimed write decodes JSON through `writes::decode_body`. This
//! one cannot: the body is `multipart/form-data`, and it is unparseable without
//! the boundary, which lives in the `Content-Type` header. That is why
//! `native::Request` carries the header at all.
//!
//! It is also why `proxy.rs` has a second body cap. Go reads the request through
//! a `MaxBytesReader` at 100 MiB and hands it to `ParseMultipartForm`, which
//! keeps 10 MiB in memory and spills the rest to temp files; the seam buffers
//! the whole body instead, because a native handler is handed `&[u8]`. The
//! trade is deliberate and is documented at [`crate::proxy`]: a 100 MiB
//! allocation on an upload, against refusing a file the server would accept.
//!
//! # Failing before the file exists
//!
//! `Err` means "forward to Go", and Go would then write the file itself. So
//! nothing fallible may run once the destination exists: the response bytes are
//! built first, the directory is created before the file, and a partial file is
//! removed before the failure is returned. A forward after a completed write
//! would leave two uploads on disk and hand the second path to the user.

use std::path::{Path, PathBuf};

use axum::http::Method;
use serde::Serialize;

use super::writes::{finish, WriteError};

/// The route, named so `proxy.rs` can size the body cap without duplicating it.
pub const PATH: &str = "/api/uploads";

/// `maxUploadSize`: what Go's `MaxBytesReader` allows.
const MAX_UPLOAD_SIZE: usize = 100 << 20;

/// `writeJSON(w, 200, map[string]string{"path": destPath})` — one key.
#[derive(Serialize)]
struct UploadResponse {
    path: String,
}

/// This module's entry in `native::ENDPOINTS`.
pub const ENDPOINT: super::Endpoint = super::Endpoint {
    name: "uploads",
    claims,
    serve,
};

fn claims(method: &Method, path: &str) -> bool {
    method == Method::POST && path == PATH
}

fn serve(ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    let _ = ctx;
    finish(upload(req.content_type, req.body, &uploads_dir()?))
}

/// `appConfig.TmpUploadsDir()`.
fn uploads_dir() -> Result<PathBuf, String> {
    Ok(crate::paths::data_dir()
        .ok_or("no home directory to resolve the data dir")?
        .join("tmp-uploads"))
}

/// `handleUploadFile`.
fn upload(content_type: &str, body: &[u8], upload_dir: &Path) -> Result<super::Answer, WriteError> {
    // Go's cap is enforced by `MaxBytesReader`, whose error surfaces from
    // `ParseMultipartForm` — so an oversized body and a malformed one produce
    // the *same* 400 message. Reproducing that means checking the size here
    // rather than answering something more informative.
    if body.len() > MAX_UPLOAD_SIZE {
        return Err(WriteError::BadRequest(
            "file too large or invalid multipart form".to_string(),
        ));
    }
    let Some(boundary) = multipart_boundary(content_type) else {
        return Err(WriteError::BadRequest(
            "file too large or invalid multipart form".to_string(),
        ));
    };

    // `r.FormFile("file")`: the first part named `file`. Go's error for a
    // missing one is its own message, distinct from the parse failure above.
    let Some(part) = find_part(body, &boundary, "file") else {
        return Err(WriteError::BadRequest(
            "missing required field: file".to_string(),
        ));
    };

    let ext = sanitize_extension(&part.filename);
    let name = format!("{}-{}{}", unix_millis(), uuid::Uuid::new_v4(), ext);

    let dest = super::gopath::clean(&super::gopath::join(&[
        &upload_dir.to_string_lossy(),
        &name,
    ]));

    // Go's traversal guard. It cannot fire on a generated name, and it is
    // reproduced rather than reasoned away: it is the check that would catch a
    // later change to how the name is built.
    let prefix = format!(
        "{}{}",
        super::gopath::clean(&upload_dir.to_string_lossy()),
        std::path::MAIN_SEPARATOR
    );
    if !dest.starts_with(&prefix) {
        return Err(WriteError::BadRequest("invalid filename".to_string()));
    }

    // Built before anything exists on disk. See the module header.
    let answer = super::gojson::to_vec(&UploadResponse { path: dest.clone() })
        .map_err(|e| WriteError::Fallback(format!("encoding upload response: {e}")))?;

    create_upload_dir(upload_dir)?;

    // `os.Create` then `io.Copy`, with Go's cleanup: a failed copy removes the
    // partial file. Here that also makes the forward safe.
    std::fs::write(&dest, part.content).map_err(|e| {
        let _ = std::fs::remove_file(&dest);
        WriteError::Fallback(format!("failed to save file {dest:?}: {e}"))
    })?;

    // Nothing below this line may fail.
    //
    // `handleUpload`'s own line, with Go's three keys. `path` is the generated
    // destination under the uploads dir rather than the name the user typed,
    // and the response body returns it anyway — see
    // `writes::service_log_convention`.
    log::info!(
        "file uploaded path={:?} size={} extension={:?}",
        dest,
        part.content.len(),
        ext
    );
    Ok(super::Answer::json(answer))
}

/// `os.MkdirAll(uploadDir, 0o750)`.
///
/// The mode is not decoration: the uploaded file's path is handed to the model
/// and the directory sits in the user's data dir, so group-writable or
/// world-readable would both be wrong. `create_dir_all` has no mode argument,
/// so the permissions are set afterwards on Unix — and only when this call
/// created the directory, so an existing one the user chmod'd is left alone.
fn create_upload_dir(dir: &Path) -> Result<(), WriteError> {
    if dir.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| WriteError::Fallback(format!("failed to create upload directory: {e}")))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o750))
            .map_err(|e| WriteError::Fallback(format!("setting upload dir mode: {e}")))?;
    }
    Ok(())
}

fn unix_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// `sanitizeExtension`: the extension of the final path element, and only when
/// every character after the dot is ASCII alphanumeric.
///
/// This is the security boundary of the route. The stored name is generated, so
/// the caller's filename reaches the filesystem through this function and
/// nothing else — a `..`, a separator, a second dot or a NUL all yield `""`
/// rather than being cleaned, because the rule is an allowlist.
fn sanitize_extension(filename: &str) -> String {
    let ext = go_ext(&go_base(filename));
    // `cleaned` is the extension without its dot, and an **empty** one passes:
    // `Base("")` is `"."`, whose `Ext` is `"."`, so an upload with no filename
    // at all would be stored as `<millis>-<uuid>.` on the Go side. Unreachable
    // from the handler — a part with no filename is not a file to `FormFile` —
    // but this function reproduces Go rather than the handler's use of it.
    if ext[1.min(ext.len())..]
        .chars()
        .all(|c| c.is_ascii_alphanumeric())
    {
        ext
    } else {
        String::new()
    }
}

/// `filepath.Ext`: the suffix from the final dot in the final element, `""`
/// when there is none.
///
/// Written as Go writes it — scanning back and stopping at a separator — rather
/// than as `rfind('.')`, because the two differ on `a.png/b`, where Go's scan
/// stops at the `/` and answers `""` while an unbounded `rfind` would answer
/// `.png/b`.
fn go_ext(path: &str) -> String {
    for (i, c) in path.char_indices().rev() {
        if c == '/' {
            break;
        }
        if c == '.' {
            return path[i..].to_string();
        }
    }
    String::new()
}

/// `filepath.Base`: strip trailing separators, take everything after the last
/// one, and answer `.` for the empty string.
///
/// **Only `/` is a separator**, because this is the Unix `filepath` and the Go
/// server runs the same one. A Windows-shaped `..\..\x.exe` is therefore one
/// element to both — which is safe for a different reason: the extension is an
/// allowlist, so anything the backslashes could smuggle in is rejected by the
/// alphanumeric check rather than by the split.
fn go_base(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return if path.is_empty() {
            ".".into()
        } else {
            "/".into()
        };
    }
    match trimmed.rfind('/') {
        Some(i) => trimmed[i + 1..].to_string(),
        None => trimmed.to_string(),
    }
}

// ─── Multipart ────────────────────────────────────────────────────────────────

/// One part of the body: what `FormFile` returns.
struct Part<'a> {
    filename: String,
    content: &'a [u8],
}

/// `mime.ParseMediaType`, narrowed to what this route needs: the `boundary`
/// parameter of a `multipart/form-data` content type.
///
/// A quoted value is unquoted, because that is how a boundary containing a
/// space or a colon has to be sent and `ParseMediaType` accepts both forms.
fn multipart_boundary(content_type: &str) -> Option<String> {
    let mut parts = content_type.split(';');
    let media_type = parts.next()?.trim();
    if !media_type.eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    for param in parts {
        let (key, value) = param.split_once('=')?;
        if !key.trim().eq_ignore_ascii_case("boundary") {
            continue;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);
        if value.is_empty() {
            return None;
        }
        return Some(value.to_string());
    }
    None
}

/// The first part whose `name` is `want`, with its `filename`.
///
/// A hand-written reader rather than a crate, for one reason: every multipart
/// crate in the ecosystem is built around an async byte *stream*, and this
/// handler runs on `spawn_blocking` with the whole body already in memory. The
/// grammar it has to cover is correspondingly small — RFC 7578 delimiters, one
/// header block, one body — and the cases that are easy to get wrong all have
/// tests: a preamble before the first delimiter, `\r\n` inside the content, a
/// part with no filename, and the closing `--`.
fn find_part<'a>(body: &'a [u8], boundary: &str, want: &str) -> Option<Part<'a>> {
    let delimiter = format!("\r\n--{boundary}");
    // The first delimiter may open the body with no preceding CRLF, so the scan
    // starts from a synthetic one rather than special-casing position 0.
    let mut buffer = Vec::with_capacity(body.len() + 2);
    buffer.extend_from_slice(b"\r\n");
    buffer.extend_from_slice(body);

    let mut cursor = 0usize;
    loop {
        let start = find(&buffer[cursor..], delimiter.as_bytes())? + cursor + delimiter.len();
        // `--` here closes the body; anything else is a part, after the CRLF
        // that ends the delimiter line (transport padding may sit between).
        if buffer[start..].starts_with(b"--") {
            return None;
        }
        let header_start = find(&buffer[start..], b"\r\n")? + start + 2;
        let header_end = find(&buffer[header_start..], b"\r\n\r\n")? + header_start;
        let headers = std::str::from_utf8(&buffer[header_start..header_end]).ok()?;
        let content_start = header_end + 4;
        let content_end = find(&buffer[content_start..], delimiter.as_bytes())? + content_start;

        if let Some((name, filename)) = content_disposition(headers) {
            // **A part with no filename is not a file.** `multipart.readForm`
            // puts it in `Form.Value`, not `Form.File`, so `FormFile("file")`
            // returns `ErrMissingFile` and Go answers 400 — even though a part
            // named `file` is right there. Matching on the name alone would
            // accept a request Go rejects.
            if name == want && !filename.is_empty() {
                // The content is a slice of `buffer`, which is local — but it
                // is offset by exactly the two synthetic bytes, so the same
                // range of `body` is the same bytes without a copy.
                return Some(Part {
                    filename,
                    content: &body[content_start - 2..content_end - 2],
                });
            }
        }
        cursor = content_end;
    }
}

/// The `name` and `filename` of a `Content-Disposition: form-data` header.
fn content_disposition(headers: &str) -> Option<(String, String)> {
    let line = headers
        .split("\r\n")
        .find(|l| l.to_ascii_lowercase().starts_with("content-disposition:"))?;
    let (mut name, mut filename) = (None, String::new());
    for param in line.split(';').skip(1) {
        let Some((key, value)) = param.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);
        match key.trim().to_ascii_lowercase().as_str() {
            "name" => name = Some(value.to_string()),
            "filename" => filename = value.to_string(),
            _ => {}
        }
    }
    Some((name?, filename))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    const BOUNDARY: &str = "----WebKitFormBoundaryABC123";

    fn body(parts: &[(&str, Option<&str>, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, filename, content) in parts {
            out.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
            let disposition = match filename {
                Some(f) => {
                    format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\n")
                }
                None => format!("Content-Disposition: form-data; name=\"{name}\"\r\n"),
            };
            out.extend_from_slice(disposition.as_bytes());
            out.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
            out.extend_from_slice(content);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        out
    }

    fn content_type() -> String {
        format!("multipart/form-data; boundary={BOUNDARY}")
    }

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    #[test]
    fn an_upload_writes_the_file_and_answers_its_absolute_path() {
        let dir = dir();
        let target = dir.path().join("tmp-uploads");
        let answer = upload(
            &content_type(),
            &body(&[("file", Some("photo.PNG"), b"\x89PNG\r\n\x1a\ndata")]),
            &target,
        )
        .expect("upload");

        assert_eq!(answer.status, StatusCode::OK);
        let json = String::from_utf8(answer.body.expect("body")).expect("utf-8");
        let path = json
            .trim_end()
            .strip_prefix(r#"{"path":""#)
            .and_then(|s| s.strip_suffix(r#""}"#))
            .expect("one-key response");

        assert!(path.starts_with(&target.to_string_lossy().to_string()));
        assert!(path.ends_with(".PNG"), "the case of the extension survives");
        // The stored bytes are the part's, `\r\n` inside the content included —
        // a reader that stopped at the first CRLF would truncate every binary
        // file at its first line break.
        assert_eq!(
            std::fs::read(path).expect("stored file"),
            b"\x89PNG\r\n\x1a\ndata"
        );
    }

    /// The whole security boundary of the route. The stored name is generated,
    /// so the caller's filename reaches the filesystem through this and nothing
    /// else — and the rule is an allowlist, so anything unexpected is dropped
    /// rather than cleaned.
    #[test]
    fn the_extension_allowlist_drops_everything_that_is_not_alphanumeric() {
        for (input, want) in [
            ("photo.png", ".png"),
            ("photo.PNG", ".PNG"),
            ("archive.tar.gz", ".gz"),
            ("report.2026", ".2026"),
            ("noextension", ""),
            // `Base("")` is `"."` and `Ext(".")` is `"."`. Surprising, and Go's.
            ("", "."),
            (".hidden", ".hidden"),
            // A traversal attempt: `Base` takes the last element, so there is
            // no extension left to take.
            ("../../etc/passwd", ""),
            // Backslashes are not separators to the Unix `filepath`, so this is
            // one element — and `.exe` is what Go extracts from it too.
            ("..\\..\\windows\\system32\\cmd.exe", ".exe"),
            // `Ext` stops at the separator, so there is no extension here at
            // all — an unbounded `rfind('.')` would have answered `.png/../x`.
            ("evil.png/../x", ""),
            ("evil.p g", ""),
            ("evil.p-g", ""),
            ("evil.p\0g", ""),
            ("trailing.", "."),
        ] {
            assert_eq!(sanitize_extension(input), want, "for {input:?}");
        }
    }

    /// `filepath.Ext` after `filepath.Base`, and the separator handling that
    /// makes a Windows-shaped name safe on a Unix server.
    /// `filepath.Base` and `filepath.Ext`, including the two answers that look
    /// like bugs: `Base("")` is `"."`, and `Ext` stops at a separator.
    #[test]
    fn base_and_ext_are_gos() {
        assert_eq!(go_base("a/b/c.png"), "c.png");
        assert_eq!(go_base("a/b/"), "b");
        assert_eq!(go_base("c.png"), "c.png");
        assert_eq!(go_base(""), ".");
        assert_eq!(go_base("/"), "/");
        // Not a separator here: this is the Unix filepath, as on the server.
        assert_eq!(go_base("a\\b\\c.png"), "a\\b\\c.png");

        assert_eq!(go_ext("c.png"), ".png");
        assert_eq!(go_ext("a.tar.gz"), ".gz");
        assert_eq!(go_ext("noext"), "");
        assert_eq!(go_ext("."), ".");
        assert_eq!(go_ext("a.png/b"), "");
    }

    #[test]
    fn a_missing_file_field_and_a_malformed_body_have_different_messages() {
        let dir = dir();
        let target = dir.path().join("tmp-uploads");

        let err = upload(
            &content_type(),
            &body(&[("other", Some("x.png"), b"data")]),
            &target,
        )
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.message(), "missing required field: file");

        // No boundary at all is the parse failure, which shares its message
        // with the size cap because Go's MaxBytesReader surfaces through
        // ParseMultipartForm.
        let err = upload("application/json", b"{}", &target).unwrap_err();
        assert_eq!(err.message(), "file too large or invalid multipart form");

        // …and nothing was created on either path.
        assert!(!target.exists());
    }

    #[test]
    fn a_body_over_the_cap_is_the_same_400_go_gives() {
        let dir = dir();
        let err = upload(
            &content_type(),
            &vec![0u8; MAX_UPLOAD_SIZE + 1],
            &dir.path().join("tmp-uploads"),
        )
        .unwrap_err();
        assert_eq!(err.message(), "file too large or invalid multipart form");
    }

    #[test]
    fn the_boundary_is_read_from_the_content_type_in_both_spellings() {
        assert_eq!(
            multipart_boundary("multipart/form-data; boundary=abc").as_deref(),
            Some("abc")
        );
        assert_eq!(
            multipart_boundary("Multipart/Form-Data; Boundary=\"a b\"").as_deref(),
            Some("a b")
        );
        assert_eq!(multipart_boundary("multipart/form-data").as_deref(), None);
        assert_eq!(
            multipart_boundary("multipart/form-data; boundary=").as_deref(),
            None
        );
        assert_eq!(multipart_boundary("application/json").as_deref(), None);
        assert_eq!(multipart_boundary("").as_deref(), None);
    }

    /// A preamble before the first delimiter is legal and some clients send
    /// one. Starting the scan at byte zero would miss the first part.
    #[test]
    fn a_preamble_before_the_first_delimiter_is_skipped() {
        let mut raw = b"This is a multipart message.\r\n".to_vec();
        raw.extend_from_slice(&body(&[("file", Some("a.txt"), b"hello")]));
        let part = find_part(&raw, BOUNDARY, "file").expect("part");
        assert_eq!(part.content, b"hello");
        assert_eq!(part.filename, "a.txt");
    }

    /// The field is selected by `name`, and value parts are skipped on the way.
    #[test]
    fn the_part_is_selected_by_name_past_the_value_fields() {
        let raw = body(&[
            ("caption", None, b"a picture"),
            ("file", Some("b.bin"), b"bytes"),
        ]);
        let part = find_part(&raw, BOUNDARY, "file").expect("part");
        assert_eq!(part.content, b"bytes");
        assert_eq!(part.filename, "b.bin");
    }

    /// A part named `file` with **no filename** is a form *value* to Go, not a
    /// file: `multipart.readForm` puts it in `Form.Value`, so `FormFile` returns
    /// `ErrMissingFile` and the handler 400s. Matching on the name alone would
    /// have accepted a request the server rejects, and stored a file called
    /// `<millis>-<uuid>.` for it.
    #[test]
    fn a_file_part_without_a_filename_is_not_a_file() {
        let dir = dir();
        let target = dir.path().join("tmp-uploads");
        let err = upload(&content_type(), &body(&[("file", None, b"bytes")]), &target).unwrap_err();
        assert_eq!(err.message(), "missing required field: file");
        assert!(!target.exists());
    }

    #[test]
    fn an_empty_file_is_stored_rather_than_rejected() {
        let dir = dir();
        let target = dir.path().join("tmp-uploads");
        let answer = upload(
            &content_type(),
            &body(&[("file", Some("e.txt"), b"")]),
            &target,
        )
        .expect("upload");
        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(std::fs::read_dir(&target).expect("dir").count(), 1);
    }

    /// Two uploads in the same millisecond must not collide — which is what the
    /// UUID in the name is for, since the timestamp alone would not.
    #[test]
    fn two_uploads_get_distinct_names() {
        let dir = dir();
        let target = dir.path().join("tmp-uploads");
        for _ in 0..2 {
            upload(
                &content_type(),
                &body(&[("file", Some("x.bin"), b"z")]),
                &target,
            )
            .expect("upload");
        }
        assert_eq!(std::fs::read_dir(&target).expect("dir").count(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn the_upload_directory_is_created_0750() {
        use std::os::unix::fs::PermissionsExt;
        let dir = dir();
        let target = dir.path().join("tmp-uploads");
        upload(
            &content_type(),
            &body(&[("file", Some("x.bin"), b"z")]),
            &target,
        )
        .expect("upload");
        let mode = std::fs::metadata(&target)
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o750);
    }

    #[test]
    fn only_the_upload_post_is_claimed() {
        assert!(claims(&Method::POST, "/api/uploads"));
        assert!(!claims(&Method::GET, "/api/uploads"));
        assert!(!claims(&Method::POST, "/api/uploads/"));
        assert!(!claims(&Method::POST, "/api/upload"));
    }

    /// #335: `handleUpload`'s own line. The path is the generated destination
    /// under the uploads dir, which the response body returns anyway.
    #[test]
    fn an_upload_logs_its_path_size_and_extension() {
        crate::native::writes::testlog::install();
        let dir = dir();
        let target = dir.path().join("logged-uploads");
        upload(
            &content_type(),
            &body(&[("file", Some("photo.PNG"), b"\x89PNG\r\n\x1a\ndata")]),
            &target,
        )
        .expect("upload");

        let found = crate::native::writes::testlog::matching("file uploaded path=");
        let line = found
            .iter()
            .find(|line| line.contains("logged-uploads"))
            .unwrap_or_else(|| panic!("no line for this upload: {found:?}"));
        assert!(line.starts_with("INFO "), "{line}");
        assert!(line.contains(r#"extension=".PNG""#), "{line}");
        assert!(line.contains("size=12"), "{line}");
    }
}
