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
//! `POST /api/fs/mkdir` creates a directory and stays with Go. So does
//! `POST /api/uploads`: **there is no upload read path** — `internal/api/uploads.go`
//! registers one route and it writes a multipart body to disk.

use axum::http::Method;
use serde::Serialize;

use super::gopath;
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

/// The listing only. Platform-independent on purpose — see [`serve`].
fn claims(method: &Method, path: &str) -> bool {
    method == Method::GET && path == "/api/fs"
}

fn serve(_ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    // Windows `filepath` is a different algorithm and unverified here, so the
    // request forwards. This is the seam's own mechanism for "cannot answer",
    // and it keeps the platform decision in one readable place rather than
    // making the route vanish from the registry.
    if !cfg!(unix) {
        return Err("the fs listing is not ported for Windows path semantics".to_string());
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

    #[test]
    fn only_the_listing_is_claimed() {
        assert!(claims(&Method::GET, "/api/fs"));
        assert!(!claims(&Method::POST, "/api/fs"));
        assert!(!claims(&Method::POST, "/api/fs/mkdir"));
        assert!(!claims(&Method::GET, "/api/fs/mkdir"));
        assert!(!claims(&Method::GET, "/api/fs/"));
        // Uploads has no read at all — one route, and it writes.
        assert!(!claims(&Method::POST, "/api/uploads"));
        assert!(!claims(&Method::GET, "/api/uploads"));
    }
}
