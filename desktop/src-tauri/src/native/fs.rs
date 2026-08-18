//! `GET /api/fs` — the directory listing behind the working-directory pickers.
//!
//! Mirrors `handleFSList` (`internal/api/filesystem.go`).
//!
//! **This reads the user's filesystem, so it must not widen what Go exposes.**
//! Three deliberate narrowings travel with it, and dropping any one shows the
//! user something the Go server would not have:
//!
//! 1. **Directories only.** `handleFSList` skips every non-directory entry, and
//!    it decides with `DirEntry.IsDir()`, which reads the type bit `readdir`
//!    returned and **does not follow symlinks**. A symlink pointing at a
//!    directory is therefore *excluded*. Rust's `DirEntry::file_type()` has the
//!    same semantics; `metadata()` does not — it follows, and using it would add
//!    entries Go omits.
//! 2. **`~` is expanded only when it is the whole path.** `""` and `"~"` become
//!    the home directory; `"~/Projects"` does not. It stays literal, `Clean`
//!    leaves it relative, and the read fails — which is Go's answer and the one
//!    the frontend is written against.
//! 3. **No traversal guard beyond `Clean`.** Go applies none, and adding one
//!    here would refuse paths the sidecar serves.
//!
//! **One accepted divergence: a filename that is not valid UTF-8.** Both
//! languages mangle it — Go's JSON encoder substitutes U+FFFD and so does
//! `to_string_lossy` — but they count differently: Go emits one replacement per
//! invalid *byte*, while Rust emits one per maximal invalid *subsequence*, so
//! `\xe0\xa0` is two characters to Go and one here. Neither answer is a usable
//! path, and reproducing Go's policy would mean hand-rolling the conversion for
//! a case the picker cannot act on, so this is documented rather than fixed.
//!
//! **Answered on Unix only.** The endpoint is `filepath.Clean`/`Dir`/`Join`
//! wrapped in a response, and Windows `filepath` strips a volume name before
//! cleaning and accepts both separators — a different algorithm, with no Windows
//! machine in this loop to verify a port of it against. The route is still
//! *claimed* there and [`serve`] returns `Err`, which the seam forwards: gating
//! `claims` instead would leave a registry entry that claims nothing, and the
//! two registry tests exist precisely to catch that shape. See [`super::gopath`].
//!
//! `POST /api/fs/mkdir` is here too (#296). It was deferred by #293 as one of
//! the two routes that escaped every category — ~20 lines, no database, no
//! Go-side state, and `gopath::clean` already existed — so the rule "a route
//! moves only when Rust can reproduce every effect it has" admits it outright:
//! its one effect is a directory on disk.
//!
//! `POST /api/uploads` is **not** here either, and it is not Go's any more: it
//! has no read path at all — `internal/api/uploads.go` registers one route and
//! it writes a multipart body to disk — so it got its own module rather than
//! joining a listing endpoint it shares nothing with. See [`super::uploads`].

use axum::http::Method;
use serde::{Deserialize, Serialize};

use super::gopath;
use super::writes::{decode_body, finish, WriteError};
use crate::paths;

/// One listed directory. Mirrors `api.fsEntry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FsEntry {
    pub name: String,
    /// Always true: the handler filters everything else out. Kept because the
    /// field is on the wire and the frontend reads it.
    pub is_dir: bool,
    pub path: String,
}

/// Mirrors `api.fsListResponse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FsListResponse {
    pub path: String,
    pub parent: String,
    /// `make([]fsEntry, 0, …)` on the Go side, so an empty directory is `[]`
    /// rather than `null` — the opposite of the notification log, and the
    /// difference is whether the slice was preallocated.
    pub entries: Vec<FsEntry>,
}

/// `handleFSList`.
///
/// Errors mean "fall back": Go answers 404 for a missing path, 400 for anything
/// else unreadable and 500 when the home directory cannot be resolved, each with
/// its own body. Reproducing those here would be three more strings to keep in
/// step for no gain while the sidecar can answer them itself.
pub fn list(raw_path: &str) -> Result<FsListResponse, String> {
    // `""` and `"~"` — and nothing else — mean the home directory.
    let expanded = if raw_path.is_empty() || raw_path == "~" {
        paths::home()
            .ok_or("could not determine home directory")?
            .to_string_lossy()
            .into_owned()
    } else {
        raw_path.to_string()
    };

    let clean = gopath::clean(&expanded);

    let mut entries = Vec::new();
    let reader =
        std::fs::read_dir(&clean).map_err(|e| format!("cannot read directory {clean:?}: {e}"))?;
    for entry in reader {
        let entry = entry.map_err(|e| format!("reading an entry of {clean:?}: {e}"))?;
        // `file_type`, never `metadata`: Go's `DirEntry.IsDir` does not follow
        // symlinks, so a link to a directory is not listed.
        let file_type = entry
            .file_type()
            .map_err(|e| format!("stat-ing an entry of {clean:?}: {e}"))?;
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        entries.push(FsEntry {
            path: gopath::join(&[&clean, &name]),
            name,
            is_dir: true,
        });
    }

    // `os.ReadDir` returns entries **sorted by filename**; `std::fs::read_dir`
    // returns them in whatever order the directory yields. Without this the
    // response is right and its order is not, which is the kind of diff that
    // passes on a small directory and fails on a big one.
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    // At the filesystem root `Dir` answers the root itself, and Go spells that
    // out rather than relying on it.
    let parent = gopath::dir(&clean);
    let parent = if parent == clean {
        clean.clone()
    } else {
        parent
    };

    Ok(FsListResponse {
        path: clean,
        parent,
        entries,
    })
}

// ─── `POST /api/fs/mkdir` ─────────────────────────────────────────────────────

/// `api.fsMkdirRequest`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct MkdirRequest {
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    path: String,
}

/// `map[string]string{"path": clean}` — one key, so nothing to sort.
#[derive(Debug, Serialize)]
struct MkdirResponse {
    path: String,
}

/// `handleFSMkdir`.
///
/// Three things about it are easy to get wrong, and all three are Go's, not
/// this port's:
///
/// - **`200`, not `201`.** Every other create in this API answers 201; this one
///   goes through `writeJSON(w, http.StatusOK, …)`.
/// - **The traversal guard is `strings.Contains(clean, "..")`, a substring test
///   on the *cleaned* path.** `Clean` has already removed every `..` element a
///   rooted path could have, so what survives the check is only a **filename**
///   containing two dots — `/home/u/..hidden` is refused, and so is
///   `/tmp/a..b`. That is a live directory name a user can type, and refusing
///   it is the behaviour the frontend is written against.
/// - **`0750` on every directory it creates**, which `std::fs::create_dir_all`
///   does not do: it uses `0777 & !umask`. A directory the user's own umask
///   would have made world-readable is a divergence with a security direction,
///   so this goes through `DirBuilder` with the mode set.
///
/// The response body is built **before** the directory is created, per the
/// write path's rule that nothing fallible may run after the effect. Here the
/// consequence would in fact be benign — `MkdirAll` is idempotent, so the
/// forward would re-run it to the same result — but the rule is cheaper to
/// keep than to reason about each time.
pub fn mkdir(body: &[u8]) -> Result<super::Answer, WriteError> {
    let req = decode_body::<MkdirRequest>(body)?;
    if req.path.is_empty() {
        return Err(WriteError::BadRequest("path is required".to_string()));
    }

    let clean = gopath::clean(&req.path);
    // `filepath.IsAbs` is `strings.HasPrefix(path, "/")` on Unix.
    if !clean.starts_with('/') || clean.contains("..") {
        return Err(WriteError::BadRequest("invalid path".to_string()));
    }

    let encoded = super::gojson::to_vec(&MkdirResponse {
        path: clean.clone(),
    })
    .map_err(|e| WriteError::Fallback(format!("encoding mkdir response: {e}")))?;

    // Nothing below this line may return `Fallback`.
    create_dir_all_0750(&clean)
        .map_err(|e| WriteError::Fallback(format!("creating {clean:?}: {e}")))?;

    Ok(super::Answer::json(encoded))
}

/// `os.MkdirAll(path, 0750)`.
#[cfg(unix)]
fn create_dir_all_0750(path: &str) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o750)
        .create(path)
}

#[cfg(not(unix))]
fn create_dir_all_0750(_path: &str) -> std::io::Result<()> {
    // Unreachable: `serve` refuses this route off Unix before it is called.
    Err(std::io::Error::other("not ported for Windows"))
}

/// The `path` query parameter. No rule of its own on top of the decoding —
/// `""` already means the home directory, which is what an absent key gives.
pub fn path_param(query: &str) -> String {
    super::query::value(query, "path")
}

// ─── The seam ─────────────────────────────────────────────────────────────────

/// This module's entry in `native::ENDPOINTS`.
pub const ENDPOINT: super::Endpoint = super::Endpoint {
    name: "fs",
    claims,
    serve,
};

/// The listing and the one write. Platform-independent on purpose — see
/// [`serve`].
fn claims(method: &Method, path: &str) -> bool {
    match *method {
        Method::GET => path == "/api/fs",
        Method::POST => path == PATH_MKDIR,
        _ => false,
    }
}

/// The write route, named so `claims` and `serve` cannot disagree about it.
const PATH_MKDIR: &str = "/api/fs/mkdir";

fn serve(_ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    // Windows `filepath` is a different algorithm and unverified here. Until
    // #278 this forwarded and the sidecar answered; with it gone the honest
    // answer is a 501 naming the gap, not a listing built with Unix path
    // arithmetic — `gopath::dir` on `C:\Users\u` finds no `/` and answers
    // `"."`, so the picker would silently browse the wrong directory.
    if !cfg!(unix) {
        return super::Answer::error(
            axum::http::StatusCode::NOT_IMPLEMENTED,
            "the filesystem browser is not supported on Windows in this build",
        );
    }
    if req.path == PATH_MKDIR {
        return finish(mkdir(req.body));
    }
    let listing = list(&path_param(req.query))?;
    let body = super::gojson::to_vec(&listing).map_err(|e| format!("encoding fs listing: {e}"))?;
    Ok(super::Answer::json(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory tree with the three shapes the handler distinguishes: real
    /// subdirectories, a plain file, and a symlink pointing at a directory.
    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        // Created out of alphabetical order so the sort is doing work.
        for name in ["zulu", "alpha", "Mike", "bravo"] {
            std::fs::create_dir(root.join(name)).expect("subdir");
        }
        std::fs::write(root.join("notes.txt"), b"x").expect("file");
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("alpha"), root.join("link-to-alpha"))
            .expect("symlink");
        dir
    }

    fn names(listing: &FsListResponse) -> Vec<&str> {
        listing.entries.iter().map(|e| e.name.as_str()).collect()
    }

    /// `os.ReadDir` sorts by filename; `std::fs::read_dir` does not. Byte order,
    /// so an uppercase name sorts before every lowercase one.
    #[test]
    fn entries_are_sorted_by_name_the_way_read_dir_sorts() {
        let dir = tree();
        let listing = list(dir.path().to_str().expect("utf8 path")).expect("listing");
        assert_eq!(names(&listing), vec!["Mike", "alpha", "bravo", "zulu"]);
    }

    /// Files are skipped, and so is a symlink to a directory — `DirEntry.IsDir`
    /// reads the type bit rather than following the link, and `metadata()`
    /// would have added an entry Go omits.
    #[test]
    fn files_and_symlinked_directories_are_both_excluded() {
        let dir = tree();
        let listing = list(dir.path().to_str().expect("utf8 path")).expect("listing");
        assert!(!names(&listing).contains(&"notes.txt"));
        assert!(
            !names(&listing).contains(&"link-to-alpha"),
            "a symlink to a directory must not be listed: {:?}",
            names(&listing)
        );
    }

    /// Every entry's path is `Join(clean, name)`, and `is_dir` is always true
    /// because everything else was filtered out.
    #[test]
    fn each_entry_carries_its_joined_path() {
        let dir = tree();
        let root = dir.path().to_str().expect("utf8 path");
        let listing = list(root).expect("listing");
        for entry in &listing.entries {
            assert!(entry.is_dir);
            assert_eq!(entry.path, gopath::join(&[&listing.path, &entry.name]));
        }
    }

    /// An empty directory is `[]`, not `null` — the Go slice is preallocated
    /// with `make`, unlike the notification log's.
    #[test]
    fn an_empty_directory_is_an_empty_array() {
        let dir = tempfile::tempdir().expect("temp dir");
        let listing = list(dir.path().to_str().expect("utf8 path")).expect("listing");
        assert!(listing.entries.is_empty());
        let body = super::super::gojson::to_vec(&listing).expect("encode");
        assert!(
            String::from_utf8(body)
                .expect("utf8")
                .contains(r#""entries":[]"#),
            "an empty listing must be [] rather than null"
        );
    }

    /// Only the bare forms expand. `~/Projects` stays literal, `Clean` leaves it
    /// relative, and the read fails — which is what Go answers.
    #[test]
    fn only_a_bare_tilde_and_the_empty_path_mean_home() {
        let home = paths::home().expect("a home directory");
        let home = home.to_string_lossy().into_owned();

        for raw in ["", "~"] {
            let listing = list(raw).expect("home listing");
            assert_eq!(listing.path, gopath::clean(&home), "{raw:?}");
        }

        assert!(
            list("~/definitely-not-a-real-directory").is_err(),
            "a trailing-path tilde must not be expanded"
        );
    }

    /// The root is its own parent, spelled out rather than left to `Dir`.
    #[test]
    fn the_filesystem_root_is_its_own_parent() {
        let listing = list("/").expect("root listing");
        assert_eq!(listing.path, "/");
        assert_eq!(listing.parent, "/");
    }

    #[test]
    fn a_nested_path_reports_its_cleaned_parent() {
        let dir = tree();
        let root = dir.path().to_str().expect("utf8 path");
        // The `..` is resolved lexically before anything is read.
        let listing = list(&format!("{root}/alpha/../bravo")).expect("listing");
        assert_eq!(listing.path, format!("{root}/bravo"));
        assert_eq!(listing.parent, root);
    }

    /// Field order is the Go struct's declaration order.
    #[test]
    fn the_response_shape_is_the_go_struct_order() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(dir.path().join("only")).expect("subdir");
        let root = dir.path().to_str().expect("utf8 path");
        let body = super::super::gojson::to_vec(&list(root).expect("listing")).expect("encode");
        assert_eq!(
            String::from_utf8(body).expect("utf8"),
            format!(
                "{{\"path\":\"{root}\",\"parent\":\"{}\",\
                 \"entries\":[{{\"name\":\"only\",\"is_dir\":true,\"path\":\"{root}/only\"}}]}}\n",
                gopath::dir(root)
            )
        );
    }

    #[test]
    fn the_path_parameter_is_decoded_the_way_go_decodes_it() {
        assert_eq!(path_param(""), "");
        assert_eq!(path_param("path=/home/u"), "/home/u");
        // Percent-decoded, including the separators a real path carries.
        assert_eq!(path_param("path=%2Fhome%2Fu%20x"), "/home/u x");
        // First value for a repeated key.
        assert_eq!(path_param("path=/a&path=/b"), "/a");
        assert_eq!(path_param("other=1&path=/a"), "/a");
    }

    // ─── `POST /api/fs/mkdir` (#296) ──────────────────────────────────────────

    fn mkdir_body(dir: &str) -> String {
        format!(
            r#"{{"path":{}}}"#,
            serde_json::to_string(dir).expect("json")
        )
    }

    /// The happy path: parents are created, the answer is **200** (not the 201
    /// every other create in this API uses), and the body carries the *cleaned*
    /// path rather than the one that was sent.
    #[test]
    fn mkdir_creates_parents_and_answers_the_cleaned_path() {
        let root = tempfile::tempdir().expect("temp dir");
        let target = format!("{}/a/b/c", root.path().to_str().expect("utf8"));

        let answer = mkdir(mkdir_body(&target).as_bytes()).expect("mkdir");
        assert_eq!(answer.status, super::super::StatusCode::OK);
        assert_eq!(
            String::from_utf8(answer.body.expect("body")).expect("utf-8"),
            format!("{{\"path\":\"{target}\"}}\n")
        );
        assert!(std::path::Path::new(&target).is_dir());

        // `MkdirAll` is idempotent, which is what makes the forward-on-error
        // arm safe.
        assert!(mkdir(mkdir_body(&target).as_bytes()).is_ok());
    }

    /// `Clean` runs **before** the guard, so a `..` that resolves away is fine
    /// and the directory that gets created is the resolved one.
    #[test]
    fn a_dot_dot_that_cleans_away_is_accepted() {
        let root = tempfile::tempdir().expect("temp dir");
        let root = root.path().to_str().expect("utf8");
        let sent = format!("{root}/x/../y");

        let answer = mkdir(mkdir_body(&sent).as_bytes()).expect("mkdir");
        let body = String::from_utf8(answer.body.expect("body")).expect("utf-8");
        assert_eq!(body, format!("{{\"path\":\"{root}/y\"}}\n"));
        assert!(std::path::Path::new(&format!("{root}/y")).is_dir());
        assert!(!std::path::Path::new(&format!("{root}/x")).exists());
    }

    /// The guard is `strings.Contains(clean, "..")` — a **substring** test, not
    /// an element test — so a filename with two dots in it is refused even
    /// though it traverses nothing. Reproduced rather than improved: it is a
    /// name a user can type, and the frontend is written against this answer.
    #[test]
    fn a_filename_containing_two_dots_is_refused_the_way_go_refuses_it() {
        let root = tempfile::tempdir().expect("temp dir");
        let root = root.path().to_str().expect("utf8");

        for name in ["..hidden", "a..b"] {
            let target = format!("{root}/{name}");
            let err = mkdir(mkdir_body(&target).as_bytes()).unwrap_err();
            assert_eq!(err.message(), "invalid path", "{name}");
            assert_eq!(err.status(), super::super::StatusCode::BAD_REQUEST);
            assert!(!std::path::Path::new(&target).exists(), "{name}");
        }
    }

    #[test]
    fn a_relative_path_is_refused_and_an_absent_one_is_a_different_message() {
        // `filepath.IsAbs` on Unix is a leading slash and nothing more.
        let err = mkdir(br#"{"path":"relative/x"}"#).unwrap_err();
        assert_eq!(err.message(), "invalid path");

        // The empty check runs first, so it wins over the cleaned `"."` that
        // would otherwise fail `IsAbs`.
        for body in [
            &br#"{}"#[..],
            &br#"{"path":""}"#[..],
            &br#"{"path":null}"#[..],
        ] {
            let err = mkdir(body).unwrap_err();
            assert_eq!(err.message(), "path is required", "{:?}", body);
            assert_eq!(err.status(), super::super::StatusCode::BAD_REQUEST);
        }

        // A `null` body is Go's zero value — no decode error — so it reaches
        // the same validation rather than the decoder's 400.
        assert_eq!(mkdir(b"null").unwrap_err().message(), "path is required");

        // Genuinely malformed, and an array, are the decoder's own 400.
        for body in [&b""[..], &b"{"[..], &b"[]"[..], &br#"["/tmp/x"]"#[..]] {
            assert_eq!(
                mkdir(body).unwrap_err(),
                WriteError::InvalidBody,
                "{body:?}"
            );
        }
    }

    /// `0750` on **every** directory created, where `std::fs::create_dir_all`
    /// would have used `0777 & !umask`. The difference has a security
    /// direction, so it is asserted rather than assumed.
    #[cfg(unix)]
    #[test]
    fn every_created_directory_gets_gos_mode() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("temp dir");
        let umask = current_umask(root.path());
        let root = root.path().to_str().expect("utf8");
        mkdir(mkdir_body(&format!("{root}/outer/inner")).as_bytes()).expect("mkdir");

        for dir in [format!("{root}/outer"), format!("{root}/outer/inner")] {
            let mode = std::fs::metadata(&dir).expect("stat").permissions().mode();
            // The umask applies to both implementations alike; 0750 under the
            // usual 022 is 0750.
            assert_eq!(mode & 0o777, 0o750 & !umask, "{dir}");
        }
    }

    /// Read the effective umask **without touching it**: a directory created
    /// with the default mode is `0o777 & !umask`, so the mask falls out of the
    /// mode.
    ///
    /// `umask(2)` has no getter, and the set-and-restore trick that stands in
    /// for one is process-global — these 600-odd tests share one binary and run
    /// on many threads, several of them creating temp files whose mode is
    /// derived from the umask at that instant. A two-syscall window where it
    /// reads `0o777` is a `tempfile` created mode `0000` in another test, and a
    /// failure nobody could attribute.
    #[cfg(unix)]
    fn current_umask(root: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;

        let probe = root.join(".umask-probe");
        std::fs::create_dir(&probe).expect("probe dir");
        let mode = std::fs::metadata(&probe)
            .expect("stat")
            .permissions()
            .mode()
            & 0o777;
        0o777 & !mode
    }

    #[test]
    fn the_listing_and_the_mkdir_are_claimed_and_nothing_else_is() {
        assert!(claims(&Method::GET, "/api/fs"));
        assert!(claims(&Method::POST, "/api/fs/mkdir"));
        // Each route for its own method only, so the wrong pairing still
        // forwards and gets chi's own 405.
        assert!(!claims(&Method::POST, "/api/fs"));
        assert!(!claims(&Method::GET, "/api/fs/mkdir"));
        assert!(!claims(&Method::DELETE, "/api/fs/mkdir"));
        assert!(!claims(&Method::GET, "/api/fs/"));
        assert!(!claims(&Method::POST, "/api/fs/mkdir/deeper"));
        // Uploads has no read at all — one route, and it writes.
        assert!(!claims(&Method::POST, "/api/uploads"));
        assert!(!claims(&Method::GET, "/api/uploads"));
    }
}
