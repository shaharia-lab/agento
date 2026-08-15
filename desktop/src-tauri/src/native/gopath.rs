//! Go's `path/filepath`, for the Unix rules.
//!
//! `GET /api/fs` is little more than `Clean`, `Dir` and `Join` wrapped in a
//! response, so porting it means porting those — and they are not Rust's
//! `std::path`. Three differences matter here:
//!
//! - **`Clean` resolves `..` lexically, not against the filesystem.** It keeps a
//!   leading `..` on a relative path and *drops* one that would escape a rooted
//!   path, so `Clean("/a/../..") == "/"`. Rust's `Path::components` keeps `..`
//!   in both cases, deliberately, because it refuses to guess about symlinks.
//! - **`Dir` is `Clean` of everything before the last separator**, so a bare
//!   name answers `"."` and `Dir("/") == "/"`.
//! - **`Join` cleans its result**, so `Join("/a", "../b") == "/b"` — an element
//!   can escape the one before it.
//!
//! Every one of those produces a path the working-directory picker then offers
//! the user, so a divergence is a real answer about the wrong directory rather
//! than a cosmetic one. `desktop/parity/gopath_vectors.json` is generated from
//! Go and read by both languages' tests, so these functions are pinned to what
//! Go actually does rather than to what this comment believes.
//!
//! **Unix only.** Windows `filepath` is a different algorithm — a volume name is
//! stripped before cleaning and both separators are accepted — and there is no
//! Windows machine in this loop to verify it on. [`super::fs`] therefore still
//! claims its route there but answers `Err`, so the request forwards to the
//! sidecar.

/// Go's `filepath.Clean`, transcribed from `path/filepath/path.go`.
///
/// The `lazybuf` in the original is an allocation optimization; the algorithm is
/// the loop, and it is reproduced branch for branch rather than approximated by
/// splitting on separators — a split-and-rejoin gets `""` (which is `"."`) and
/// the rooted-`..` case wrong, and both reach this code from a user-typed path.
pub fn clean(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }

    let bytes = path.as_bytes();
    let rooted = bytes[0] == b'/';
    let n = bytes.len();

    // Go's `lazybuf`: a fixed buffer plus a write cursor, kept rather than
    // simplified to a `Vec` with `push`/`truncate`. The `..` branch below reads
    // `buf[w]` — the byte *at* the cursor, one past the new end — which a
    // truncating buffer cannot offer, and getting that wrong leaves a doubled
    // separator on `Clean("/a/b/../c/")`. Found by the Go vectors, not by
    // reading the source.
    let mut buf = vec![0u8; n + 1];
    let mut w = 0usize;
    let mut r = 0usize;
    let mut dotdot = 0usize;
    if rooted {
        buf[w] = b'/';
        w += 1;
        r = 1;
        dotdot = 1;
    }

    while r < n {
        if bytes[r] == b'/' {
            // Empty path element.
            r += 1;
        } else if bytes[r] == b'.' && (r + 1 == n || bytes[r + 1] == b'/') {
            // "." element.
            r += 1;
        } else if bytes[r] == b'.'
            && r + 1 < n
            && bytes[r + 1] == b'.'
            && (r + 2 == n || bytes[r + 2] == b'/')
        {
            // ".." element: back up one, if we can.
            r += 2;
            if w > dotdot {
                w -= 1;
                while w > dotdot && buf[w] != b'/' {
                    w -= 1;
                }
            } else if !rooted {
                // Cannot back up: keep the "..". A rooted path has nowhere to
                // go, so its ".." is dropped entirely — that is the asymmetry.
                if w > 0 {
                    buf[w] = b'/';
                    w += 1;
                }
                buf[w] = b'.';
                w += 1;
                buf[w] = b'.';
                w += 1;
                dotdot = w;
            }
        } else {
            // Real path element: add a separator if needed, then copy it.
            if (rooted && w != 1) || (!rooted && w != 0) {
                buf[w] = b'/';
                w += 1;
            }
            while r < n && bytes[r] != b'/' {
                buf[w] = bytes[r];
                w += 1;
                r += 1;
            }
        }
    }

    if w == 0 {
        return ".".to_string();
    }
    buf.truncate(w);
    // Every byte came from `path`, which is a `&str`, and the separators are
    // ASCII — so the buffer is still valid UTF-8. Multi-byte sequences are
    // copied whole by the element loop, which only ever stops on `/`.
    String::from_utf8(buf).unwrap_or_else(|_| path.to_string())
}

/// Go's `filepath.Dir`: `Clean` of everything up to and including the last
/// separator. A path with no separator answers `"."`, not `""`.
pub fn dir(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut i = bytes.len() as isize - 1;
    while i >= 0 && bytes[i as usize] != b'/' {
        i -= 1;
    }
    clean(&path[..(i + 1) as usize])
}

/// Go's `filepath.Join`: concatenate the non-empty elements with a separator,
/// then `Clean` the result — which is why an element may escape the one before
/// it, and why an empty element vanishes instead of leaving a `//`.
pub fn join(elems: &[&str]) -> String {
    let mut joined = String::new();
    for elem in elems {
        if elem.is_empty() {
            continue;
        }
        if joined.is_empty() {
            joined.push_str(elem);
        } else {
            joined.push('/');
            joined.push_str(elem);
        }
    }
    if joined.is_empty() {
        return String::new();
    }
    clean(&joined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Case {
        value: String,
        want: String,
    }

    #[derive(Deserialize)]
    struct JoinCase {
        elems: Vec<String>,
        want: String,
    }

    #[derive(Deserialize)]
    struct Vectors {
        clean: Vec<Case>,
        dir: Vec<Case>,
        join: Vec<JoinCase>,
    }

    fn vectors() -> Vectors {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../parity/gopath_vectors.json");
        let raw = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("reading {path}: {e} — regenerate it from Go"));
        serde_json::from_str(&raw).expect("parsing gopath vectors")
    }

    /// The whole point of the file: Rust is asserted against what Go actually
    /// produced, not against what this port believes Go produces.
    #[test]
    fn clean_matches_the_go_vectors() {
        let v = vectors();
        assert!(v.clean.len() >= 30, "vectors look truncated");
        for case in v.clean {
            assert_eq!(clean(&case.value), case.want, "Clean({:?})", case.value);
        }
    }

    #[test]
    fn dir_matches_the_go_vectors() {
        for case in vectors().dir {
            assert_eq!(dir(&case.value), case.want, "Dir({:?})", case.value);
        }
    }

    #[test]
    fn join_matches_the_go_vectors() {
        for case in vectors().join {
            let elems: Vec<&str> = case.elems.iter().map(String::as_str).collect();
            assert_eq!(join(&elems), case.want, "Join({:?})", case.elems);
        }
    }

    /// The two cases a split-and-rejoin implementation gets wrong, called out
    /// so a future rewrite fails on them specifically rather than somewhere in
    /// the vector loop.
    #[test]
    fn the_two_cases_a_naive_implementation_misses() {
        // Empty is ".", not "".
        assert_eq!(clean(""), ".");
        // A rooted ".." has nowhere to go and is dropped; an unrooted one is kept.
        assert_eq!(clean("/a/../.."), "/");
        assert_eq!(clean("a/../.."), "..");
    }

    /// Multi-byte path elements survive the byte-level loop intact.
    #[test]
    fn non_ascii_elements_round_trip() {
        assert_eq!(clean("/home/ü/Projekte/../.claude"), "/home/ü/.claude");
        assert_eq!(join(&["/home/ü", "文档"]), "/home/ü/文档");
    }
}
