//! Go's `path/filepath`, for **both** the Unix and the Windows rules.
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
//! than a cosmetic one. `parity/gopath_vectors.json` (Unix) and
//! `parity/gopath_windows_vectors.json` (Windows) are generated from Go and read
//! by this module's tests, so these functions are pinned to what Go actually
//! does rather than to what this comment believes.
//!
//! # Two rule sets, selected by target and tested on every host (#374)
//!
//! Windows `filepath` is a **different algorithm**, not a tweak: a volume name
//! is split off and cleaned separately, `\` and `/` are both separators, the
//! output is normalised to `\`, a `\\?\` or `\??\` prefix is left alone, and
//! `Join` has its own drive-relative and UNC rules before it re-cleans. Both
//! implementations are therefore compiled everywhere and both vector files are
//! asserted everywhere; only [`clean`], [`dir`], [`join`] and [`base`] dispatch,
//! and they dispatch on `cfg(windows)` rather than on a runtime flag — a runtime
//! switch would let a Unix build be handed Windows rules by accident, and it is
//! the *target* that decides what a path on this machine means.
//!
//! That is what lets a Linux CI run catch a Windows regression, which matters
//! because there is still no Windows machine in this loop: `windows_rules` in
//! `.github/workflows/ci.yml` compiles and runs the `cfg(windows)` arms, but the
//! path arithmetic itself is proven on every runner by the vectors.
//!
//! **The Unix arms below are byte-for-byte what they were** — the Unix vectors
//! are frozen and pass unchanged. A diff there is a bug in a change, never a
//! vector to regenerate.
//!
//! **On the port's fidelity.** The Windows half is transcribed from Go's own
//! source branch for branch — `internal/filepathlite/path.go` (the `lazybuf`,
//! `Clean`, `Base`, `Dir`, `VolumeName`), `internal/filepathlite/path_windows.go`
//! (`volumeNameLen`, `postClean` and friends) and `path/filepath/path_windows.go`
//! (`Join`'s Windows body). The `lazybuf`'s laziness is **load-bearing rather
//! than an allocation trick**: `postClean` returns early when the buffer was
//! never allocated, i.e. when the output has not diverged from the input, so an
//! eager buffer turns `Clean(`\??\a`)` into `\.\??\a`. That is exactly the shape
//! of bug the vectors exist to catch.

// ─── Unix rules ───────────────────────────────────────────────────────────────

/// Go's `filepath.Clean`, transcribed from `path/filepath/path.go`.
///
/// The `lazybuf` in the original is an allocation optimization; the algorithm is
/// the loop, and it is reproduced branch for branch rather than approximated by
/// splitting on separators — a split-and-rejoin gets `""` (which is `"."`) and
/// the rooted-`..` case wrong, and both reach this code from a user-typed path.
pub fn clean_unix(path: &str) -> String {
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
pub fn dir_unix(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut i = bytes.len() as isize - 1;
    while i >= 0 && bytes[i as usize] != b'/' {
        i -= 1;
    }
    clean_unix(&path[..(i + 1) as usize])
}

/// Go's `filepath.Join`: concatenate the non-empty elements with a separator,
/// then `Clean` the result — which is why an element may escape the one before
/// it, and why an empty element vanishes instead of leaving a `//`.
pub fn join_unix(elems: &[&str]) -> String {
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
    clean_unix(&joined)
}

/// Go's `filepath.IsAbs` on Unix: `strings.HasPrefix(path, "/")`, and nothing
/// else. Spelled out here so the Windows arm has a sibling rather than an
/// inline `starts_with('/')` at the one call site that would then be wrong.
pub fn is_abs_unix(path: &str) -> bool {
    path.starts_with('/')
}

/// Go's `filepath.Base`: the last element, with trailing separators stripped
/// first. `""` is `"."` and a path of only separators is `"/"` — neither is the
/// empty string a `rsplit('/')` answers, which is why this exists rather than a
/// split at the one call site that wanted it (`sessions::projects`).
pub fn base_unix(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let mut b = path.as_bytes();
    while !b.is_empty() && b[b.len() - 1] == b'/' {
        b = &b[..b.len() - 1];
    }
    // Go throws away the volume name here; on Unix it is always empty.
    let mut i = b.len() as isize - 1;
    while i >= 0 && b[i as usize] != b'/' {
        i -= 1;
    }
    if i >= 0 {
        b = &b[(i + 1) as usize..];
    }
    if b.is_empty() {
        return "/".to_string();
    }
    from_utf8_or(b, path)
}

// ─── Windows rules ────────────────────────────────────────────────────────────

/// `os.PathSeparator` on Windows. `Clean` normalises every separator to this
/// one, which is why its output never contains a `/`.
const SEPARATOR_WINDOWS: u8 = b'\\';

/// Go's `filepathlite.IsPathSeparator` on Windows: **both** slashes separate.
fn is_sep_windows(c: u8) -> bool {
    c == b'\\' || c == b'/'
}

/// Go's `filepathlite.toUpper` — ASCII only, deliberately: it backs
/// `pathHasPrefixFold`, which compares against `\\.\UNC` and friends.
fn to_upper_ascii(c: u8) -> u8 {
    if c.is_ascii_lowercase() {
        c - (b'a' - b'A')
    } else {
        c
    }
}

/// Go's `filepathlite.FromSlash` on Windows: every `/` becomes `\`. Multiple
/// slashes become multiple separators — it is not a `Clean`.
fn from_slash_windows(b: &[u8]) -> String {
    let mut out = b.to_vec();
    for c in out.iter_mut() {
        if *c == b'/' {
            *c = SEPARATOR_WINDOWS;
        }
    }
    // `/` and `\` are both ASCII, so replacing one with the other cannot split
    // a multi-byte sequence.
    String::from_utf8(out).unwrap_or_else(|_| String::from_utf8_lossy(b).into_owned())
}

fn from_utf8_or(b: &[u8], fallback: &str) -> String {
    std::str::from_utf8(b)
        .map(str::to_string)
        .unwrap_or_else(|_| fallback.to_string())
}

/// Go's `filepathlite.pathHasPrefixFold`: case-insensitive, every separator
/// equivalent, and if `s` is longer than `prefix` the next byte must separate.
fn path_has_prefix_fold(s: &[u8], prefix: &[u8]) -> bool {
    if s.len() < prefix.len() {
        return false;
    }
    for i in 0..prefix.len() {
        if is_sep_windows(prefix[i]) {
            if !is_sep_windows(s[i]) {
                return false;
            }
        } else if to_upper_ascii(prefix[i]) != to_upper_ascii(s[i]) {
            return false;
        }
    }
    if s.len() > prefix.len() && !is_sep_windows(s[prefix.len()]) {
        return false;
    }
    true
}

/// Go's `filepathlite.uncLen`: the volume prefix of a UNC path runs to the
/// second separator after the host.
fn unc_len(path: &[u8], prefix_len: usize) -> usize {
    let mut count = 0;
    let mut i = prefix_len;
    while i < path.len() {
        if is_sep_windows(path[i]) {
            count += 1;
            if count == 2 {
                return i;
            }
        }
        i += 1;
    }
    path.len()
}

/// Go's `filepathlite.cutPath`: the index of the first separator, if any.
fn cut_path(path: &[u8]) -> Option<usize> {
    path.iter().position(|&c| is_sep_windows(c))
}

/// Go's `filepathlite.volumeNameLen`, transcribed whole.
///
/// Five shapes, and the order of the arms is the specification: a drive letter
/// (`C:`), a Local Device UNC (`\\.\UNC\host\share`), a Local or Root Local
/// Device path (`\\.\`, `\\?\`, `\??\`), and a plain UNC (`\\host\share`).
/// The `\\?\` arm is why `Clean(`\\?\C:\`)` keeps its trailing separator: the
/// component after the prefix is part of the volume, so there is nothing left
/// for `Clean` to trim.
fn volume_name_len(path: &[u8]) -> usize {
    if path.len() >= 2 && path[1] == b':' {
        // Path starts with a drive letter. Go does not check that the letter is
        // in A-Z, and neither does this.
        return 2;
    }
    if path.is_empty() || !is_sep_windows(path[0]) {
        return 0;
    }
    if path_has_prefix_fold(path, br"\\.\UNC") {
        return unc_len(path, br"\\.\UNC\".len());
    }
    if path_has_prefix_fold(path, br"\\.")
        || path_has_prefix_fold(path, br"\\?")
        || path_has_prefix_fold(path, br"\??")
    {
        if path.len() == 3 {
            return 3; // exactly \\.
        }
        return match cut_path(&path[4..]) {
            None => path.len(),
            Some(i) => 4 + i,
        };
    }
    if path.len() >= 2 && is_sep_windows(path[1]) {
        return unc_len(path, 2);
    }
    0
}

/// Go's `filepath.VolumeName` on Windows: the leading volume, `\`-normalised.
pub fn volume_name_windows(path: &str) -> String {
    let b = path.as_bytes();
    from_slash_windows(&b[..volume_name_len(b)])
}

/// Go's `filepathlite.lazybuf`, and it is lazy for a *behavioural* reason —
/// see the module header. `buf` stays `None` until the output diverges from the
/// input, and `postClean` keys on exactly that.
struct LazyBuf<'a> {
    path: &'a [u8],
    buf: Option<Vec<u8>>,
    w: usize,
    vol_and_path: &'a [u8],
    vol_len: usize,
}

impl<'a> LazyBuf<'a> {
    fn new(path: &'a [u8], vol_and_path: &'a [u8], vol_len: usize) -> Self {
        Self {
            path,
            buf: None,
            w: 0,
            vol_and_path,
            vol_len,
        }
    }

    fn index(&self, i: usize) -> u8 {
        match &self.buf {
            Some(buf) => buf[i],
            None => self.path[i],
        }
    }

    fn append(&mut self, c: u8) {
        if self.buf.is_none() {
            if self.w < self.path.len() && self.path[self.w] == c {
                self.w += 1;
                return;
            }
            // Go's `make([]byte, len(b.path))` — the tail is zeroed, and
            // `post_clean` walks the whole slice rather than `buf[..w]`, so the
            // length here is part of the algorithm.
            let mut buf = vec![0u8; self.path.len()];
            buf[..self.w].copy_from_slice(&self.path[..self.w]);
            self.buf = Some(buf);
        }
        let buf = self.buf.as_mut().expect("just allocated");
        buf[self.w] = c;
        self.w += 1;
    }

    fn prepend(&mut self, prefix: &[u8]) {
        let buf = self
            .buf
            .as_mut()
            .expect("prepend is only reached with a buffer");
        buf.splice(0..0, prefix.iter().copied());
        self.w += prefix.len();
    }

    fn finish(&self) -> Vec<u8> {
        match &self.buf {
            None => self.vol_and_path[..self.vol_len + self.w].to_vec(),
            Some(buf) => {
                let mut out = self.vol_and_path[..self.vol_len].to_vec();
                out.extend_from_slice(&buf[..self.w]);
                out
            }
        }
    }
}

/// Go's `filepathlite.postClean`: stop a *relative* path from cleaning itself
/// into a rooted one.
///
/// Two rewrites, both security-shaped rather than cosmetic. `a/../c:` would
/// otherwise clean to `c:`, a drive-relative path; and `\a\..\??\c:\x` would
/// clean to `\??\c:\x`, a Root Local Device path equivalent to `c:\x`. Go
/// inserts `.\` and `\.` respectively.
fn post_clean(out: &mut LazyBuf) {
    if out.vol_len != 0 || out.buf.is_none() {
        return;
    }
    // Go ranges the whole buffer, not `buf[..w]`; the tail is zeros, which are
    // neither a separator nor a colon, so the loop is unchanged by that — but
    // it is transcribed as written rather than tightened.
    let buf = out.buf.as_ref().expect("checked above");
    let mut colon_first = false;
    for &c in buf.iter() {
        if is_sep_windows(c) {
            break;
        }
        if c == b':' {
            colon_first = true;
            break;
        }
    }
    if colon_first {
        out.prepend(&[b'.', SEPARATOR_WINDOWS]);
        return;
    }
    let buf = out.buf.as_ref().expect("checked above");
    if buf.len() >= 3 && is_sep_windows(buf[0]) && buf[1] == b'?' && buf[2] == b'?' {
        out.prepend(&[SEPARATOR_WINDOWS, b'.']);
    }
}

/// Go's `filepath.Clean` under the Windows rules.
///
/// The shape that surprises: a **bare volume is not a root**. `Clean("c:")` is
/// `c:.` — the drive's current directory — while `Clean(`c:\`)` is `c:\`. The
/// volume is split off before the loop and never rewritten except by
/// `FromSlash`, which is what keeps `Clean("//host/share/../x")` at
/// `\\host\share\x` rather than eating the share.
pub fn clean_windows(path: &str) -> String {
    let original = path.as_bytes();
    let vol_len = volume_name_len(original);
    let p = &original[vol_len..];
    if p.is_empty() {
        if vol_len > 1 && is_sep_windows(original[0]) && is_sep_windows(original[1]) {
            // should be UNC
            return from_slash_windows(original);
        }
        return format!("{path}.");
    }
    let rooted = is_sep_windows(p[0]);

    let n = p.len();
    let mut out = LazyBuf::new(p, original, vol_len);
    let mut r = 0usize;
    let mut dotdot = 0usize;
    if rooted {
        out.append(SEPARATOR_WINDOWS);
        r = 1;
        dotdot = 1;
    }

    while r < n {
        if is_sep_windows(p[r]) {
            // empty path element
            r += 1;
        } else if p[r] == b'.' && (r + 1 == n || is_sep_windows(p[r + 1])) {
            // . element
            r += 1;
        } else if p[r] == b'.' && p[r + 1] == b'.' && (r + 2 == n || is_sep_windows(p[r + 2])) {
            // `p[r + 1]` cannot be out of range: the arm above already caught
            // every `.` with nothing after it, so reaching here with `p[r]` a
            // dot means `r + 1 < n`. Go relies on the same ordering.
            //
            // .. element: remove to last separator
            r += 2;
            if out.w > dotdot {
                // can backtrack
                out.w -= 1;
                while out.w > dotdot && !is_sep_windows(out.index(out.w)) {
                    out.w -= 1;
                }
            } else if !rooted {
                // cannot backtrack, but not rooted, so append .. element.
                if out.w > 0 {
                    out.append(SEPARATOR_WINDOWS);
                }
                out.append(b'.');
                out.append(b'.');
                dotdot = out.w;
            }
        } else {
            // real path element: add a separator if needed, then copy it.
            if (rooted && out.w != 1) || (!rooted && out.w != 0) {
                out.append(SEPARATOR_WINDOWS);
            }
            while r < n && !is_sep_windows(p[r]) {
                out.append(p[r]);
                r += 1;
            }
        }
    }

    // Turn empty string into "."
    if out.w == 0 {
        out.append(b'.');
    }

    post_clean(&mut out); // avoid creating absolute paths on Windows
    let bytes = out.finish();
    from_slash_windows(&bytes)
}

/// Go's `filepath.Dir` under the Windows rules.
///
/// `Dir` never crosses the volume — the scan stops at `len(vol)` — so
/// `Dir(`c:\`)` is `c:\` and `Dir(`\\host\share`)` is `\\host\share`. The
/// `len(vol) > 2` arm answers the bare volume for a UNC share whose remainder
/// cleans to `"."`, where a drive would answer `c:.`.
pub fn dir_windows(path: &str) -> String {
    let b = path.as_bytes();
    let vol_len = volume_name_len(b);
    let vol = from_slash_windows(&b[..vol_len]);
    let mut i = b.len() as isize - 1;
    while i >= vol_len as isize && !is_sep_windows(b[i as usize]) {
        i -= 1;
    }
    let rest = &b[vol_len..(i + 1) as usize];
    // Every byte between the volume and a separator came from `path`, and both
    // bounds land on ASCII, so this slice is still valid UTF-8.
    let dir = clean_windows(&from_utf8_or(rest, path));
    if dir == "." && vol_len > 2 {
        // must be UNC
        return vol;
    }
    format!("{vol}{dir}")
}

/// Go's `filepath.Base` under the Windows rules: trailing separators stripped,
/// then the volume thrown away, then the last element.
pub fn base_windows(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let mut b = path.as_bytes();
    while !b.is_empty() && is_sep_windows(b[b.len() - 1]) {
        b = &b[..b.len() - 1];
    }
    // Go recomputes the volume on the *trimmed* path, which is why
    // `Base(`c:\`)` is `\` and not `c:`.
    b = &b[volume_name_len(b)..];
    let mut i = b.len() as isize - 1;
    while i >= 0 && !is_sep_windows(b[i as usize]) {
        i -= 1;
    }
    if i >= 0 {
        b = &b[(i + 1) as usize..];
    }
    if b.is_empty() {
        return (SEPARATOR_WINDOWS as char).to_string();
    }
    from_utf8_or(b, path)
}

/// Go's `filepath.IsAbs` under the Windows rules.
///
/// Rooted is not absolute: `\Windows` and `c:a\b` are both **false**, because
/// the first names no drive and the second is drive-*relative*. A UNC path is
/// absolute as soon as it has a volume, so `\\host\share` is true with no
/// trailing separator at all.
pub fn is_abs_windows(path: &str) -> bool {
    let b = path.as_bytes();
    let l = volume_name_len(b);
    if l == 0 {
        return false;
    }
    // If the volume name starts with a double slash, this is an absolute path.
    if is_sep_windows(b[0]) && is_sep_windows(b[1]) {
        return true;
    }
    let rest = &b[l..];
    if rest.is_empty() {
        return false;
    }
    is_sep_windows(rest[0])
}

/// Go's `filepath.Join` under the Windows rules — `path/filepath/path_windows.go`.
///
/// Three special cases before the final `Clean`, and none of them is derivable
/// from the Unix version: an element following a separator has *its* leading
/// separators stripped, so joining onto `\` cannot manufacture a UNC path; an
/// element following a `:` is appended with no separator, so `Join("C:", "a")`
/// is `C:a` (drive-relative) while `Join("C:", "\a")` is `C:\a`; and `\` joined
/// with `??…` gets an extra `.\` so the result is not a Root Local Device path.
pub fn join_windows(elems: &[&str]) -> String {
    let mut b = String::new();
    let mut last_char = 0u8;
    for elem in elems {
        let mut e: &str = elem;
        if b.is_empty() {
            // Add the first non-empty path element unchanged.
        } else if is_sep_windows(last_char) {
            // If the path ends in a slash, strip any leading slashes from the
            // next element to avoid creating a UNC path from non-UNC elements.
            while !e.is_empty() && is_sep_windows(e.as_bytes()[0]) {
                e = &e[1..];
            }
            // If the path is \ and the next element is ??, add an extra .\ so
            // the result is \.\?? rather than \??\ (a Root Local Device path).
            if b.len() == 1
                && e.starts_with("??")
                && (e.len() == 2 || is_sep_windows(e.as_bytes()[2]))
            {
                b.push_str(".\\");
            }
        } else if last_char == b':' {
            // Keep the path relative to the current directory on a drive and
            // don't add a separator: Join(`C:`, `f`) == `C:f`.
        } else {
            b.push(SEPARATOR_WINDOWS as char);
            last_char = SEPARATOR_WINDOWS;
        }
        if !e.is_empty() {
            b.push_str(e);
            last_char = *e.as_bytes().last().expect("non-empty");
        }
    }
    if b.is_empty() {
        return String::new();
    }
    clean_windows(&b)
}

// ─── The dispatching API ──────────────────────────────────────────────────────
//
// `cfg`, not a runtime flag: what a path on this machine means is a property of
// the target, and a runtime switch would let a Unix build be handed the Windows
// rules by accident. Every caller in `native/` uses these four and is therefore
// correct on both platforms by construction.

/// `filepath.Clean` for this target.
#[cfg(not(windows))]
pub fn clean(path: &str) -> String {
    clean_unix(path)
}

/// `filepath.Clean` for this target.
#[cfg(windows)]
pub fn clean(path: &str) -> String {
    clean_windows(path)
}

/// `filepath.Dir` for this target.
#[cfg(not(windows))]
pub fn dir(path: &str) -> String {
    dir_unix(path)
}

/// `filepath.Dir` for this target.
#[cfg(windows)]
pub fn dir(path: &str) -> String {
    dir_windows(path)
}

/// `filepath.Join` for this target.
#[cfg(not(windows))]
pub fn join(elems: &[&str]) -> String {
    join_unix(elems)
}

/// `filepath.Join` for this target.
#[cfg(windows)]
pub fn join(elems: &[&str]) -> String {
    join_windows(elems)
}

/// `filepath.Base` for this target.
#[cfg(not(windows))]
pub fn base(path: &str) -> String {
    base_unix(path)
}

/// `filepath.Base` for this target.
#[cfg(windows)]
pub fn base(path: &str) -> String {
    base_windows(path)
}

/// `filepath.IsAbs` for this target.
#[cfg(not(windows))]
pub fn is_abs(path: &str) -> bool {
    is_abs_unix(path)
}

/// `filepath.IsAbs` for this target.
#[cfg(windows)]
pub fn is_abs(path: &str) -> bool {
    is_abs_windows(path)
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

    #[derive(Deserialize)]
    struct BoolCase {
        value: String,
        want: bool,
    }

    #[derive(Deserialize)]
    struct WindowsVectors {
        clean: Vec<Case>,
        dir: Vec<Case>,
        join: Vec<JoinCase>,
        base: Vec<Case>,
        volume_name: Vec<Case>,
        unix_base: Vec<Case>,
        is_abs: Vec<BoolCase>,
    }

    fn read(name: &str) -> String {
        let path = format!("{}/../parity/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {path}: {e} — regenerate it from Go"))
    }

    fn vectors() -> Vectors {
        serde_json::from_str(&read("gopath_vectors.json")).expect("parsing gopath vectors")
    }

    fn windows_vectors() -> WindowsVectors {
        serde_json::from_str(&read("gopath_windows_vectors.json"))
            .expect("parsing gopath windows vectors")
    }

    /// The whole point of the file: Rust is asserted against what Go actually
    /// produced, not against what this port believes Go produces.
    #[test]
    fn clean_matches_the_go_vectors() {
        let v = vectors();
        assert!(v.clean.len() >= 30, "vectors look truncated");
        for case in v.clean {
            assert_eq!(
                clean_unix(&case.value),
                case.want,
                "Clean({:?})",
                case.value
            );
        }
    }

    #[test]
    fn dir_matches_the_go_vectors() {
        for case in vectors().dir {
            assert_eq!(dir_unix(&case.value), case.want, "Dir({:?})", case.value);
        }
    }

    #[test]
    fn join_matches_the_go_vectors() {
        for case in vectors().join {
            let elems: Vec<&str> = case.elems.iter().map(String::as_str).collect();
            assert_eq!(join_unix(&elems), case.want, "Join({:?})", case.elems);
        }
    }

    /// The two cases a split-and-rejoin implementation gets wrong, called out
    /// so a future rewrite fails on them specifically rather than somewhere in
    /// the vector loop.
    #[test]
    fn the_two_cases_a_naive_implementation_misses() {
        // Empty is ".", not "".
        assert_eq!(clean_unix(""), ".");
        // A rooted ".." has nowhere to go and is dropped; an unrooted one is kept.
        assert_eq!(clean_unix("/a/../.."), "/");
        assert_eq!(clean_unix("a/../.."), "..");
    }

    /// Multi-byte path elements survive the byte-level loop intact.
    #[test]
    fn non_ascii_elements_round_trip() {
        assert_eq!(clean_unix("/home/ü/Projekte/../.claude"), "/home/ü/.claude");
        assert_eq!(join_unix(&["/home/ü", "文档"]), "/home/ü/文档");
    }

    // ─── Windows (#374) ───────────────────────────────────────────────────────
    //
    // Asserted on **every** host, not only on Windows: the rule set is selected
    // by target but both are compiled, so a Linux CI run catches a Windows
    // regression. That is the whole reason the two halves are not `cfg`'d out.

    #[test]
    fn windows_clean_matches_the_go_vectors() {
        let v = windows_vectors();
        assert!(v.clean.len() >= 60, "windows vectors look truncated");
        for case in v.clean {
            assert_eq!(
                clean_windows(&case.value),
                case.want,
                "Clean({:?})",
                case.value
            );
        }
    }

    #[test]
    fn windows_dir_matches_the_go_vectors() {
        for case in windows_vectors().dir {
            assert_eq!(dir_windows(&case.value), case.want, "Dir({:?})", case.value);
        }
    }

    #[test]
    fn windows_join_matches_the_go_vectors() {
        for case in windows_vectors().join {
            let elems: Vec<&str> = case.elems.iter().map(String::as_str).collect();
            assert_eq!(join_windows(&elems), case.want, "Join({:?})", case.elems);
        }
    }

    #[test]
    fn windows_base_matches_the_go_vectors() {
        for case in windows_vectors().base {
            assert_eq!(
                base_windows(&case.value),
                case.want,
                "Base({:?})",
                case.value
            );
        }
    }

    #[test]
    fn windows_volume_name_matches_the_go_vectors() {
        for case in windows_vectors().volume_name {
            assert_eq!(
                volume_name_windows(&case.value),
                case.want,
                "VolumeName({:?})",
                case.value
            );
        }
    }

    /// `filepath.Base` under the Unix rules. Its cases live in the *Windows*
    /// vector file because `gopath_vectors.json` is frozen and may not gain an
    /// array; the generator produces them from the host's real `path/filepath`,
    /// which is the Unix build.
    #[test]
    fn unix_base_matches_the_go_vectors() {
        for case in windows_vectors().unix_base {
            assert_eq!(base_unix(&case.value), case.want, "Base({:?})", case.value);
        }
    }

    #[test]
    fn windows_is_abs_matches_the_go_vectors() {
        for case in windows_vectors().is_abs {
            assert_eq!(
                is_abs_windows(&case.value),
                case.want,
                "IsAbs({:?})",
                case.value
            );
        }
    }

    /// `POST /api/fs/mkdir`'s guard is `IsAbs`, and the two rule sets disagree
    /// about every Windows path — including in the direction that matters, a
    /// *rooted but drive-less* path that must still be refused.
    #[test]
    fn is_abs_is_the_mkdir_guard_on_both_platforms() {
        assert!(is_abs_unix("/home/u/x"));
        assert!(!is_abs_unix(r"C:\Users\u\x"));

        assert!(is_abs_windows(r"C:\Users\u\x"));
        assert!(is_abs_windows(r"\\host\share"));
        // Rooted is not absolute: no drive, so `MkdirAll` would resolve it
        // against the process's current drive.
        assert!(!is_abs_windows(r"\Users\u"));
        assert!(!is_abs_windows(r"c:a\b"));
        assert!(!is_abs_windows("relative"));
    }

    /// The failure this whole issue is named for: the Unix rules applied to a
    /// Windows path answer about the wrong directory rather than failing.
    #[test]
    fn the_unix_rules_are_wrong_about_a_windows_path() {
        assert_eq!(dir_unix(r"C:\Users\u\.claude"), ".");
        assert_eq!(dir_windows(r"C:\Users\u\.claude"), r"C:\Users\u");

        assert_eq!(clean_unix(r"C:\a\b\..\c"), r"C:\a\b\..\c");
        assert_eq!(clean_windows(r"C:\a\b\..\c"), r"C:\a\c");

        // The config-dir probe's own case: a `configured` entry can only ever
        // match its own suggestion if `join` builds the same separator.
        assert_eq!(
            join_unix(&[r"C:\Users\u", ".claude-work"]),
            r"C:\Users\u/.claude-work"
        );
        assert_eq!(
            join_windows(&[r"C:\Users\u", ".claude-work"]),
            r"C:\Users\u\.claude-work"
        );
    }

    /// The `lazybuf`'s laziness is behaviour, not an allocation trick:
    /// `post_clean` returns early when the buffer was never allocated. An eager
    /// buffer rewrites a path that needed no rewriting.
    #[test]
    fn post_clean_only_fires_when_the_output_diverged() {
        // Already a Root Local Device path — the volume swallows the prefix and
        // nothing is inserted.
        assert_eq!(clean_windows(r"\??\c:\x"), r"\??\c:\x");
        // Reached by cleaning, so it is rewritten.
        assert_eq!(clean_windows(r"\a\..\??\c:\x"), r"\.\??\c:\x");
        // The colon half of the same rule.
        assert_eq!(clean_windows("a/../c:"), r".\c:");
        assert_eq!(clean_windows("foo:bar"), "foo:bar");
    }

    /// Multi-byte elements survive the Windows byte loop too, and the volume
    /// split never lands mid-character.
    #[test]
    fn windows_non_ascii_elements_round_trip() {
        assert_eq!(
            clean_windows(r"C:\Users\ü\Projekte\..\.claude"),
            r"C:\Users\ü\.claude"
        );
        assert_eq!(join_windows(&[r"C:\Users\ü", "文档"]), r"C:\Users\ü\文档");
        assert_eq!(base_windows(r"C:\Users\ü\文档"), "文档");
    }

    /// The dispatching functions answer their target's rules — the property
    /// every un-gated caller in `native/` now relies on.
    #[test]
    fn the_public_functions_follow_the_target() {
        #[cfg(windows)]
        {
            assert_eq!(dir(r"C:\Users\u\.claude"), r"C:\Users\u");
            assert_eq!(join(&[r"C:\Users\u", ".claude"]), r"C:\Users\u\.claude");
            assert_eq!(base(r"C:\a\b"), "b");
            assert_eq!(clean("C:/a/b/../c"), r"C:\a\c");
        }
        #[cfg(not(windows))]
        {
            assert_eq!(dir("/home/u/.claude"), "/home/u");
            assert_eq!(join(&["/home/u", ".claude"]), "/home/u/.claude");
            assert_eq!(base("/a/b"), "b");
            assert_eq!(clean("/a/b/../c"), "/a/c");
        }
    }
}
