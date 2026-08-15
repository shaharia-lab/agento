//! Live parity for the filesystem listing: diff `GET /api/fs` against a
//! *running* Go server, byte for byte.
//!
//! Ignored by default: it needs a real Agento instance, and CI has none.
//!
//! ```sh
//! cd desktop && eval "$(./scripts/parity-instance.sh start)"
//! (cd src-tauri && cargo test --test parity_fs -- --ignored --nocapture)
//! ./scripts/parity-instance.sh stop
//! ```
//!
//! **Never point this at the instance on :8990.** That is whatever binary the
//! developer installed, which drifts behind the repo — a stale baseline makes a
//! wrong port look verified, and a right one look broken.
//!
//! **This endpoint needs no seeding**, unlike every other parity suite here: it
//! reads the real filesystem, which is already full of the shapes that matter.
//! What it does need is *variety*, so the cases below deliberately include a
//! directory with many entries (where an unsorted listing diverges but a small
//! one might not), the home directory reached three ways, the filesystem root
//! (its own parent), and paths carrying `.`, `..` and a trailing separator.
//!
//! The suite creates one throwaway tree under the system temp dir so the
//! symlink and file cases are exercised even on a machine whose home directory
//! happens to contain neither. That is the only thing it writes, and it removes
//! it again.
//!
//! **Otherwise read-only.** It issues GETs and nothing else — no `POST /fs/mkdir`.
//!
//! Unix-only, like the port itself: the suite creates a symlink, and the route
//! answers `Err` on Windows so there would be nothing to diff.

#![cfg(unix)]

mod parity_common;

use parity_common::*;

use agento_lib::native::{fs, gojson};

/// Percent-encode a path for the query string. Only the characters a real path
/// can carry that would otherwise change the parse.
fn encode(path: &str) -> String {
    form_urlencoded::Serializer::new(String::new())
        .append_pair("path", path)
        .finish()
}

#[tokio::test]
#[ignore = "needs a running Agento instance"]
async fn the_filesystem_listing_matches_the_live_go_response() {
    let home = agento_lib::paths::home().expect("a home directory");
    let home = home.to_string_lossy().into_owned();

    // A tree the machine is not guaranteed to have otherwise: a subdirectory, a
    // plain file and a symlink pointing at a directory — the last is the case
    // `DirEntry.IsDir` excludes and `metadata()` would have included.
    let scratch = tempfile::tempdir().expect("temp dir");
    let root = scratch.path().to_string_lossy().into_owned();
    for name in ["zulu", "alpha", "Mike"] {
        std::fs::create_dir(scratch.path().join(name)).expect("subdir");
    }
    std::fs::write(scratch.path().join("notes.txt"), b"x").expect("file");
    std::os::unix::fs::symlink(
        scratch.path().join("alpha"),
        scratch.path().join("link-to-alpha"),
    )
    .expect("symlink");
    let empty = scratch.path().join("alpha").to_string_lossy().into_owned();

    let cases = [
        // The three spellings that mean the home directory, and the one that
        // does not.
        String::new(),
        "~".to_string(),
        home.clone(),
        // Enough entries that an unsorted listing cannot pass by luck.
        "/usr/lib".to_string(),
        "/etc".to_string(),
        // The root is its own parent.
        "/".to_string(),
        // Lexical cleaning: `.`, `..` and a trailing separator.
        format!("{home}/"),
        format!("{home}/./"),
        format!("{home}/../{}", file_name(&home)),
        // The throwaway tree, and one of its directories, which is empty —
        // `[]` rather than `null`.
        root.clone(),
        empty,
    ];

    for path in &cases {
        let go = fetch(&format!("/api/fs?{}", encode(path))).await;
        let native = gojson::to_vec(&fs::list(path).unwrap_or_else(|e| {
            panic!("native listing of {path:?} failed: {e}");
        }))
        .expect("encode");
        assert_identical(&format!("fs?path={path}"), &go, &native);
    }

    // The listing of a directory with many entries is where sorting shows; if
    // every case above happened to be tiny the suite would prove little.
    let go = fetch(&format!("/api/fs?{}", encode("/usr/lib"))).await;
    let entries = String::from_utf8(go)
        .expect("utf8")
        .matches("\"name\":")
        .count();
    assert!(
        entries >= 10,
        "/usr/lib listed {entries} directories — too few to exercise the sort. \
         Pick a busier directory for this machine."
    );
}

/// `filepath.Base` for the one use above.
fn file_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}
