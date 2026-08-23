//! The app log, read back for Settings → Logs.
//!
//! `lib.rs` configures `tauri_plugin_log` to write one file per install into
//! Tauri's app-log directory, 5 MiB with `KeepSome(3)` archives beside it
//! (#301). That file is the record a user is asked to send when they hit a bug
//! — every `/api` request is in it, failures at warn and writes at info — and
//! until now nothing in the app could show it to them. Asking someone to find
//! `~/.local/share/com.shaharialab.agento/logs/Agento.log` by hand is asking
//! most people for nothing.
//!
//! **These are Tauri commands rather than `/api` routes, and that is
//! deliberate.** `/api` is the surface this port mirrors from the Go server,
//! recorded route by route in `parity/read_routes.json`; the log file belongs
//! to the plugin in *this* process and has no Go counterpart, so serving it
//! there would be a permanent divergence in the one place the port keeps
//! honest. Commands registered through `invoke_handler` also bypass the ACL
//! entirely, so this needs no capability edit — see the `remote.urls` note in
//! `CLAUDE.md` for why that matters in release builds. The cost is that the
//! pane is blank in a plain browser tab (`npm run dev`), where there is no
//! log file to read either way.
//!
//! Everything below the command wrappers takes a directory and a file stem
//! rather than an `AppHandle`, so the tail arithmetic and the archive ordering
//! are testable without a window.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::Manager;

/// Most a single read may return, whatever the caller asks for. The live file
/// is capped at 5 MiB by the plugin, so this covers a whole one; the clamp is
/// there to stop a webview asking for a gigabyte of a file somebody replaced.
const MAX_READ: u64 = 8 * 1024 * 1024;

/// Least a single read may return. A tail smaller than this is not a window
/// onto anything.
const MIN_READ: u64 = 4 * 1024;

/// One file in the log directory: the live one the plugin is appending to, or
/// one of the dated archives it rotated.
#[derive(Serialize)]
pub struct LogFile {
    /// File name, which is also the handle `read_log` takes back. Never a
    /// path — see `resolve`.
    pub name: String,
    pub bytes: u64,
    /// Unix milliseconds, or null when the platform cannot say.
    pub modified_ms: Option<u64>,
    /// True for the file currently being written.
    pub live: bool,
}

#[derive(Serialize)]
pub struct LogIndex {
    /// Absolute path of the log directory, shown so a user can find the files
    /// themselves. There is no reveal-in-file-manager button: that needs an
    /// `opener:allow-open-path` capability this app does not grant.
    pub dir: String,
    /// Live file first, then archives newest to oldest.
    pub files: Vec<LogFile>,
}

/// A window onto one file. The caller keeps `next` and passes it back as
/// `from` to follow the file; `reset` says whether what came back replaces
/// what it already had or is appended to it.
#[derive(Serialize)]
pub struct LogChunk {
    pub text: String,
    /// Byte offset `text` starts at.
    pub start: u64,
    /// Byte offset to resume from. Always a line boundary, so a message the
    /// writer was midway through does not arrive as two lines.
    pub next: u64,
    /// The file's length when it was read.
    pub size: u64,
    /// True when the read did not start at the file's beginning, i.e. older
    /// lines exist above what came back.
    pub truncated: bool,
    /// True when this chunk replaces the caller's buffer rather than extending
    /// it — a first read, or a file that shrank underneath a follow because
    /// the plugin rotated it.
    pub reset: bool,
}

// --- Commands ------------------------------------------------------------

#[tauri::command]
pub fn log_files(app: tauri::AppHandle) -> Result<LogIndex, String> {
    let (dir, stem) = location(&app)?;
    Ok(LogIndex {
        dir: dir.to_string_lossy().into_owned(),
        files: index(&dir, &stem),
    })
}

#[tauri::command]
pub fn read_log(
    app: tauri::AppHandle,
    name: Option<String>,
    max_bytes: Option<u64>,
    from: Option<u64>,
) -> Result<LogChunk, String> {
    let (dir, stem) = location(&app)?;
    let path = resolve(&dir, &stem, name.as_deref())?;
    read_chunk(&path, max_bytes.unwrap_or(MIN_READ), from)
}

/// Write every log file this install still has into one file at `dest`,
/// oldest first, under a header naming the build.
///
/// One file rather than a zip or a folder because of what it is for: something
/// to drag into a GitHub issue. The destination comes from the native save
/// dialog, so the user has already chosen it.
#[tauri::command]
pub fn export_logs(app: tauri::AppHandle, dest: String) -> Result<u64, String> {
    let (dir, stem) = location(&app)?;
    let info = app.package_info();
    let header = format!(
        "# {} {} — {} {}\n# exported {}\n# source {}\n",
        info.name,
        info.version,
        std::env::consts::OS,
        std::env::consts::ARCH,
        chrono::Utc::now().to_rfc3339(),
        dir.to_string_lossy(),
    );
    export(&dir, &stem, Path::new(&dest), &header)
}

/// The directory the plugin writes to, and the stem it names its files with.
///
/// Both are read from the same places `tauri_plugin_log` reads them —
/// `app_log_dir()` and `package_info().name` — rather than being spelled out
/// again here, so a change to either cannot leave this module looking in the
/// wrong place.
fn location(app: &tauri::AppHandle) -> Result<(PathBuf, String), String> {
    let dir = app
        .path()
        .app_log_dir()
        .map_err(|e| format!("resolving the log directory: {e}"))?;
    Ok((dir, app.package_info().name.clone()))
}

// --- The filesystem half, which knows nothing about Tauri ----------------

/// Every log file in `dir`, live one first and archives newest to oldest.
///
/// A directory that cannot be listed is an empty index rather than an error:
/// before the first line is written there is no directory at all, and a first
/// run should show "nothing logged yet" instead of a failure.
pub fn index(dir: &Path, stem: &str) -> Vec<LogFile> {
    let live_name = format!("{stem}.log");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut files: Vec<LogFile> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_log_file(&name, stem) {
                return None;
            }
            let meta = entry.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some(LogFile {
                live: name == live_name,
                name,
                bytes: meta.len(),
                modified_ms: meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64),
            })
        })
        .collect();

    // The archive names are `<stem>_<YYYY-MM-DD_HH-MM-SS>.log`, zero-padded
    // throughout, so lexical order is chronological order and no timestamp has
    // to be parsed. The live file has no date at all and is pinned to the
    // front, because it is what a user opening this pane wants to see.
    files.sort_by(|a, b| b.live.cmp(&a.live).then(b.name.cmp(&a.name)));
    files
}

/// Whether `name` is one of the plugin's own files.
///
/// `.log.bak` is included: the rotator renames an archive out of the way with
/// that suffix when two rotations land in the same second, and a file it will
/// never write to again is exactly the kind that holds the line somebody is
/// looking for.
fn is_log_file(name: &str, stem: &str) -> bool {
    let Some(rest) = name.strip_prefix(stem) else {
        return false;
    };
    let Some(rest) = rest
        .strip_suffix(".log")
        .or_else(|| rest.strip_suffix(".log.bak"))
    else {
        return false;
    };
    // Either the live file (nothing between stem and extension) or a dated
    // archive (`_` then the timestamp). Anything else in this directory —
    // another target's file, a copy somebody made — is not ours to serve.
    rest.is_empty() || rest.starts_with('_')
}

/// Turn a name the webview sent into a path inside the log directory.
///
/// **The name is matched against the directory listing rather than joined onto
/// it.** `dir.join(name)` accepts `../../.ssh/id_rsa` and every other
/// traversal, and this command reads any file it is given back to the page;
/// there is no supported name it refuses, because the only names the frontend
/// ever sends came from `index`.
fn resolve(dir: &Path, stem: &str, name: Option<&str>) -> Result<PathBuf, String> {
    let files = index(dir, stem);
    let wanted = match name {
        Some(name) => files.iter().find(|f| f.name == name),
        // No name means the live file, falling back to the newest archive so
        // the pane still shows something in the window between a rotation and
        // the next line being written.
        None => files.iter().find(|f| f.live).or_else(|| files.first()),
    };
    match wanted {
        Some(file) => Ok(dir.join(&file.name)),
        None => Err("no log file yet".to_string()),
    }
}

/// Read at most `max_bytes` of `path`, either the tail or the span starting at
/// `from`, cut to whole lines at both ends.
fn read_chunk(path: &Path, max_bytes: u64, from: Option<u64>) -> Result<LogChunk, String> {
    let span = max_bytes.clamp(MIN_READ, MAX_READ);

    let mut file = File::open(path).map_err(|e| format!("opening {}: {e}", path.display()))?;
    let size = file
        .metadata()
        .map_err(|e| format!("reading {}: {e}", path.display()))?
        .len();

    // A `from` past the end means the file shrank underneath us, which on this
    // directory means the plugin rotated it: the caller's buffer describes a
    // file that no longer exists, so start over from the tail and say so.
    let (mut start, reset) = match from {
        Some(offset) if offset <= size => (offset, false),
        _ => (size.saturating_sub(span), true),
    };

    let end = size.min(start + span);
    file.seek(SeekFrom::Start(start))
        .map_err(|e| format!("seeking {}: {e}", path.display()))?;
    let mut buf = Vec::with_capacity((end - start) as usize);
    file.take(end - start)
        .read_to_end(&mut buf)
        .map_err(|e| format!("reading {}: {e}", path.display()))?;

    // A tail almost never lands on a line boundary, so drop the partial line
    // it starts with. Only when the read actually began mid-file: at offset 0
    // the first line is whole.
    let truncated = start > 0;
    if truncated && reset {
        if let Some(nl) = buf.iter().position(|b| *b == b'\n') {
            buf.drain(..=nl);
            start += nl as u64 + 1;
        }
    }

    // Stop at the last newline, so a line the writer is midway through is left
    // for the next read rather than arriving in two pieces. The exception is a
    // chunk with no newline at all *and* nothing more to wait for — a whole
    // span of one line — which would otherwise never advance.
    let mut next = start + buf.len() as u64;
    match buf.iter().rposition(|b| *b == b'\n') {
        Some(nl) => {
            buf.truncate(nl + 1);
            next = start + buf.len() as u64;
        }
        None if buf.len() as u64 == span => {}
        None => {
            buf.clear();
            next = start;
        }
    }

    Ok(LogChunk {
        // Lossy rather than strict: a log file is bytes a crash can land in the
        // middle of, and refusing to show any of it because one line is not
        // UTF-8 would be the wrong answer for the one file a user opens *after*
        // something went wrong.
        text: String::from_utf8_lossy(&buf).into_owned(),
        start,
        next,
        size,
        truncated,
        reset,
    })
}

/// Concatenate every log file into `dest`, oldest first.
fn export(dir: &Path, stem: &str, dest: &Path, header: &str) -> Result<u64, String> {
    let mut files = index(dir, stem);
    // `index` is newest-first for the pane; an export reads top to bottom.
    files.reverse();

    let out = File::create(dest).map_err(|e| format!("writing {}: {e}", dest.display()))?;
    let mut out = BufWriter::new(out);
    let mut written = header.len() as u64;
    out.write_all(header.as_bytes())
        .map_err(|e| format!("writing {}: {e}", dest.display()))?;

    for file in &files {
        let banner = format!("\n===== {} ({} bytes) =====\n", file.name, file.bytes);
        out.write_all(banner.as_bytes())
            .map_err(|e| format!("writing {}: {e}", dest.display()))?;
        written += banner.len() as u64;

        // Copied rather than read into memory: three archives plus the live
        // file is 20 MiB, and this runs on the UI's thread pool.
        let mut src =
            File::open(dir.join(&file.name)).map_err(|e| format!("reading {}: {e}", file.name))?;
        written +=
            std::io::copy(&mut src, &mut out).map_err(|e| format!("reading {}: {e}", file.name))?;
    }

    out.flush()
        .map_err(|e| format!("writing {}: {e}", dest.display()))?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn the_index_is_the_live_file_then_archives_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Agento.log", "live\n");
        write(dir.path(), "Agento_2026-08-20_09-00-00.log", "old\n");
        write(dir.path(), "Agento_2026-08-21_09-00-00.log", "newer\n");
        // Not ours, and not the app's: neither may appear.
        write(dir.path(), "Other.log", "no\n");
        write(dir.path(), "Agento.txt", "no\n");

        let names: Vec<String> = index(dir.path(), "Agento")
            .into_iter()
            .map(|f| f.name)
            .collect();
        assert_eq!(
            names,
            vec![
                "Agento.log",
                "Agento_2026-08-21_09-00-00.log",
                "Agento_2026-08-20_09-00-00.log",
            ]
        );
    }

    #[test]
    fn a_rotated_bak_file_is_still_a_log_file() {
        assert!(is_log_file("Agento.log", "Agento"));
        assert!(is_log_file("Agento_2026-08-21_09-00-00.log", "Agento"));
        assert!(is_log_file("Agento_2026-08-21_09-00-00.log.bak", "Agento"));
        // A prefix match alone would take these, and the second is what a
        // second app in the same directory would be called.
        assert!(!is_log_file("Agento-old.log", "Agento"));
        assert!(!is_log_file("AgentoOther.log", "Agento"));
        assert!(!is_log_file("Agento.log.gz", "Agento"));
    }

    /// The whole security property of `read_log`: the name is a key into the
    /// listing, never a path fragment. `dir.join(name)` would serve any file
    /// on the machine to the webview.
    #[test]
    fn a_name_outside_the_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Agento.log", "live\n");

        for name in [
            "../../../etc/passwd",
            "..",
            "/etc/passwd",
            "Agento.log/../../secret",
        ] {
            assert!(
                resolve(dir.path(), "Agento", Some(name)).is_err(),
                "{name} resolved"
            );
        }
        assert_eq!(
            resolve(dir.path(), "Agento", Some("Agento.log")).unwrap(),
            dir.path().join("Agento.log")
        );
    }

    #[test]
    fn no_name_means_the_live_file_and_an_empty_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve(dir.path(), "Agento", None).is_err());

        // Between a rotation and the next line, only the archive exists.
        write(dir.path(), "Agento_2026-08-21_09-00-00.log", "old\n");
        assert_eq!(
            resolve(dir.path(), "Agento", None).unwrap(),
            dir.path().join("Agento_2026-08-21_09-00-00.log")
        );

        write(dir.path(), "Agento.log", "live\n");
        assert_eq!(
            resolve(dir.path(), "Agento", None).unwrap(),
            dir.path().join("Agento.log")
        );
    }

    #[test]
    fn a_tail_starts_at_a_line_boundary() {
        let dir = tempfile::tempdir().unwrap();
        // Comfortably past the 4 KiB floor, so the read is a real tail.
        let body: String = (0..800).map(|i| format!("line {i:03}\n")).collect();
        write(dir.path(), "Agento.log", &body);

        let chunk = read_chunk(&dir.path().join("Agento.log"), MIN_READ, None).unwrap();
        assert!(chunk.reset);
        assert!(chunk.truncated);
        assert!(
            chunk.text.starts_with("line "),
            "partial first line: {:?}",
            &chunk.text[..20.min(chunk.text.len())]
        );
        assert!(chunk.text.ends_with("line 799\n"));
        assert_eq!(chunk.next, chunk.size);
    }

    #[test]
    fn a_whole_file_under_the_window_is_not_truncated() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Agento.log", "one\ntwo\n");

        let chunk = read_chunk(&dir.path().join("Agento.log"), MIN_READ, None).unwrap();
        assert_eq!(chunk.text, "one\ntwo\n");
        assert_eq!(chunk.start, 0);
        assert!(!chunk.truncated);
    }

    /// What following the file does: the second read returns only what was
    /// appended, and a line the writer has not finished is held back rather
    /// than delivered in halves.
    #[test]
    fn a_follow_read_returns_only_whole_new_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Agento.log");
        write(dir.path(), "Agento.log", "one\n");

        let first = read_chunk(&path, MIN_READ, None).unwrap();
        assert_eq!(first.text, "one\n");

        std::fs::write(&path, "one\ntwo\nthr").unwrap();
        let second = read_chunk(&path, MIN_READ, Some(first.next)).unwrap();
        assert!(!second.reset, "an appended file must not reset the view");
        assert_eq!(second.text, "two\n");
        assert_eq!(second.next, 8, "the partial line is left for the next read");

        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let third = read_chunk(&path, MIN_READ, Some(second.next)).unwrap();
        assert_eq!(third.text, "three\n");
    }

    /// A rotation replaces the file with a shorter one, so the caller's offset
    /// now points past the end. Reading from it would return nothing forever.
    #[test]
    fn a_file_that_shrank_resets_the_view() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Agento.log");
        write(dir.path(), "Agento.log", "one\ntwo\nthree\n");
        let first = read_chunk(&path, MIN_READ, None).unwrap();

        std::fs::write(&path, "fresh\n").unwrap();
        let after = read_chunk(&path, MIN_READ, Some(first.next)).unwrap();
        assert!(after.reset);
        assert_eq!(after.text, "fresh\n");
    }

    #[test]
    fn the_export_runs_oldest_to_newest_under_a_header() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Agento.log", "live line\n");
        write(
            dir.path(),
            "Agento_2026-08-20_09-00-00.log",
            "oldest line\n",
        );
        write(
            dir.path(),
            "Agento_2026-08-21_09-00-00.log",
            "middle line\n",
        );

        let dest = dir.path().join("out.log");
        let written = export(dir.path(), "Agento", &dest, "# header\n").unwrap();

        let out = std::fs::read_to_string(&dest).unwrap();
        assert_eq!(written, out.len() as u64);
        assert!(out.starts_with("# header\n"));
        let oldest = out.find("oldest line").unwrap();
        let middle = out.find("middle line").unwrap();
        let live = out.find("live line").unwrap();
        assert!(oldest < middle && middle < live, "{out}");
        assert!(out.contains("===== Agento.log ("));
    }
}
