use super::*;

const KEY: &str = "cleanupPeriodDays";

/// The bytes before and after `old` in `src`, which a single-value edit must
/// leave identical. `old` must occur exactly once.
fn around<'a>(src: &'a str, old: &str) -> (&'a str, &'a str) {
    assert_eq!(
        src.matches(old).count(),
        1,
        "fixture must hold {old:?} once"
    );
    let at = src.find(old).expect("present");
    (&src[..at], &src[at + old.len()..])
}

/// Everything a reader of the file could tell apart, in one fixture: nested
/// objects and arrays, keys out of order, `1e2`, `90.0`, a 20-digit integer,
/// escapes, non-ASCII text, a nested `cleanupPeriodDays`, the key's name
/// inside a string, mixed tab and space indentation, and CRLF line endings.
fn awkward() -> String {
    [
        "{\r\n",
        "\t\"zeta\": {\"cleanupPeriodDays\": 7, \"list\": [1e2, 90.0, {\"a\": []}]},\r\n",
        "  \"alpha\" :12345678901234567890,\r\n",
        "    \"cleanupPeriodDays\":   30   ,\r\n",
        "  \"note\": \"say \\\"cleanupPeriodDays\\\": 1 \\u00e9 \u{00e9}\u{6f22}\\n\",\r\n",
        "\t\"hooks\": [[], {}, \"}]\"]\r\n",
        "}\r\n",
    ]
    .concat()
}

#[test]
fn a_present_key_changes_only_its_value_bytes() {
    let src = awkward();
    let out = splice(&src, KEY, "90").expect("splice");
    let (before, after) = around(&src, "30");
    assert_eq!(out, format!("{before}90{after}"));

    for value in [
        "1e2",
        "90.0",
        "12345678901234567890",
        "\"x\"",
        "{\"a\":[1]}",
    ] {
        let out = splice(&src, KEY, value).expect("splice");
        assert_eq!(out.as_bytes(), format!("{before}{value}{after}").as_bytes());
    }
}

#[test]
fn a_one_line_file_keeps_its_bytes_around_the_value() {
    let src = r#"{"model":"opus","cleanupPeriodDays":30,"z":[1,2]}"#;
    assert_eq!(
        splice(src, KEY, "90").expect("splice"),
        r#"{"model":"opus","cleanupPeriodDays":90,"z":[1,2]}"#
    );
}

#[test]
fn a_key_spelled_with_an_escape_is_the_same_key() {
    let src = r#"{"cleanup\u0050eriodDays":30}"#;
    assert_eq!(
        splice(src, KEY, "90").expect("splice"),
        r#"{"cleanup\u0050eriodDays":90}"#
    );
}

#[test]
fn a_nested_key_or_the_name_inside_a_string_is_not_the_key() {
    let src = "{\n  \"zeta\": {\"cleanupPeriodDays\": 7},\n  \"note\": \"cleanupPeriodDays\"\n}";
    assert_eq!(
        splice(src, KEY, "90").expect("splice"),
        "{\n  \"zeta\": {\"cleanupPeriodDays\": 7},\n  \"note\": \"cleanupPeriodDays\",\n  \"cleanupPeriodDays\": 90\n}"
    );
}

#[test]
fn an_absent_key_is_appended_in_the_files_own_style() {
    for (src, want) in [
        // Indented, two members: the separator comes from the last member.
        (
            "{\n  \"a\": 1,\n  \"b\": {\"c\": 2}\n}\n",
            "{\n  \"a\": 1,\n  \"b\": {\"c\": 2},\n  \"cleanupPeriodDays\": 90\n}\n",
        ),
        // Indented, one member: the separator is what follows the brace.
        (
            "{\n    \"a\": 1\n}",
            "{\n    \"a\": 1,\n    \"cleanupPeriodDays\": 90\n}",
        ),
        // CRLF with tabs, and `" : "` around the colon.
        (
            "{\r\n\t\"a\" : 1,\r\n\t\"b\" : 2\r\n}\r\n",
            "{\r\n\t\"a\" : 1,\r\n\t\"b\" : 2,\r\n\t\"cleanupPeriodDays\" : 90\r\n}\r\n",
        ),
        // One line stays one line.
        (
            r#"{"a":1,"b":2}"#,
            r#"{"a":1,"b":2,"cleanupPeriodDays":90}"#,
        ),
        // No trailing newline, and none is added.
        ("{\"a\": 1}", "{\"a\": 1,\"cleanupPeriodDays\": 90}"),
        // The empty object.
        ("{}", "{\n  \"cleanupPeriodDays\": 90\n}"),
        ("  { }\n", "  {\n  \"cleanupPeriodDays\": 90\n}\n"),
        ("{\r\n}\r\n", "{\r\n  \"cleanupPeriodDays\": 90\r\n}\r\n"),
    ] {
        let out = splice(src, KEY, "90").expect(src);
        assert_eq!(out, want, "{src:?}");
        assert!(go_json_valid(out.as_bytes()), "{out:?}");
        // The original bytes up to the last value are a prefix of the result.
        if let Some(last) =
            scan_object(src.as_bytes()).and_then(|o| o.members.last().map(|m| m.value.1))
        {
            assert!(out.starts_with(&src[..last]), "{src:?}");
            assert!(out.ends_with(&src[last..]), "{src:?}");
        }
    }
}

#[test]
fn every_original_key_keeps_its_position_when_one_is_appended() {
    let src = awkward().replace("\"cleanupPeriodDays\":   30   ,", "\"other\":   30   ,");
    let out = splice(&src, KEY, "90").expect("splice");
    let keys = |s: &str| -> Vec<String> {
        scan_object(s.as_bytes())
            .expect("object")
            .members
            .iter()
            .map(|m| serde_json::from_str(&s[m.key.0..m.key.1]).expect("key"))
            .collect()
    };
    let mut want = keys(&src);
    want.push(KEY.to_string());
    assert_eq!(keys(&out), want);
}

#[test]
fn a_file_that_cannot_be_decided_about_is_refused() {
    for (src, err) in [
        ("not json", PatchError::NotJson),
        ("", PatchError::NotJson),
        ("{\"a\":1,}", PatchError::NotJson),
        ("{\"a\":1} // comment", PatchError::NotJson),
        (" [30]", PatchError::NotAnObject),
        ("30", PatchError::NotAnObject),
        ("\"{}\"", PatchError::NotAnObject),
        (
            r#"{"cleanupPeriodDays":1,"cleanupPeriodDays":2}"#,
            PatchError::DuplicateKey,
        ),
        (
            r#"{"cleanupPeriodDays":1,"cleanup\u0050eriodDays":2}"#,
            PatchError::DuplicateKey,
        ),
    ] {
        assert_eq!(splice(src, KEY, "90"), Err(err), "{src:?}");
    }
}

#[test]
fn the_value_must_be_one_json_value_so_it_cannot_inject_members() {
    for value in ["", "90, \"x\": 1", "90}", "{", "nope", "1 2"] {
        assert_eq!(
            splice("{\"a\":1}", KEY, value),
            Err(PatchError::InvalidValue),
            "{value:?}"
        );
    }
    assert_eq!(
        splice("{}", KEY, " 90\n").expect("surrounding space is trimmed"),
        "{\n  \"cleanupPeriodDays\": 90\n}"
    );
}

#[test]
fn a_key_needing_escapes_is_encoded() {
    assert_eq!(
        splice("{\"a\":1}", "we\"ird", "2").expect("splice"),
        r#"{"a":1,"we\"ird":2}"#
    );
}

// ─── The write, over temp dirs ────────────────────────────────────────────────

fn dir_string(dir: &tempfile::TempDir) -> String {
    dir.path().to_string_lossy().into_owned()
}

fn settings_of(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("settings.json")
}

/// A file's bytes and modification time, for "left alone" asserts.
fn state(path: &std::path::Path) -> (Vec<u8>, std::time::SystemTime) {
    let meta = std::fs::metadata(path).expect("metadata");
    (
        std::fs::read(path).expect("bytes"),
        meta.modified().expect("mtime"),
    )
}

#[test]
fn the_key_is_replaced_in_the_chosen_dir_and_the_other_dir_is_untouched() {
    let work = tempfile::tempdir().expect("temp dir");
    let home = tempfile::tempdir().expect("temp dir");
    std::fs::write(settings_of(&work), awkward()).expect("write");
    std::fs::write(settings_of(&home), awkward()).expect("write");
    let indexed = [dir_string(&home), dir_string(&work)];
    let home_before = state(&settings_of(&home));

    set_top_level_key(&indexed, &dir_string(&work), KEY, "90").expect("write");

    let src = awkward();
    let (before, after) = around(&src, "30");
    assert_eq!(
        std::fs::read(settings_of(&work)).expect("read"),
        format!("{before}90{after}").into_bytes()
    );
    assert_eq!(state(&settings_of(&home)), home_before);
}

#[cfg(unix)]
#[test]
fn an_absent_file_is_created_0600_in_a_dir_created_0700() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("temp dir");
    let dir = root
        .path()
        .join(".claude-new")
        .to_string_lossy()
        .into_owned();
    set_top_level_key(std::slice::from_ref(&dir), &dir, KEY, "90").expect("write");

    let path = settings_json_path(&dir);
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "{\n  \"cleanupPeriodDays\": 90\n}"
    );
    let mode = |p: &str| std::fs::metadata(p).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&dir), 0o700);
}

#[test]
fn every_refusal_leaves_the_file_as_it_was() {
    for (content, err) in [
        (&b"{\"a\":\"\xff\"}"[..], PatchError::NotUtf8),
        (b"{oops", PatchError::NotJson),
        (b"[30]", PatchError::NotAnObject),
        (
            br#"{"cleanupPeriodDays":1,"cleanupPeriodDays":2}"#,
            PatchError::DuplicateKey,
        ),
    ] {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(settings_of(&dir), content).expect("write");
        let before = state(&settings_of(&dir));
        let indexed = [dir_string(&dir)];

        assert_eq!(
            set_top_level_key(&indexed, &dir_string(&dir), KEY, "90"),
            Err(err)
        );
        assert_eq!(state(&settings_of(&dir)), before);
        assert_eq!(std::fs::read_dir(dir.path()).expect("list").count(), 1);
    }
}

#[test]
fn a_dir_outside_the_indexed_set_is_refused_and_not_written() {
    let indexed_dir = tempfile::tempdir().expect("temp dir");
    let other = tempfile::tempdir().expect("temp dir");
    std::fs::write(settings_of(&other), "{\"cleanupPeriodDays\":30}").expect("write");
    let before = state(&settings_of(&other));

    let indexed = [dir_string(&indexed_dir)];
    assert_eq!(
        set_top_level_key(&indexed, &dir_string(&other), KEY, "90"),
        Err(PatchError::DirNotIndexed)
    );
    assert_eq!(state(&settings_of(&other)), before);

    // A dir that does not exist is refused before anything is created.
    let missing = indexed_dir
        .path()
        .join("nope")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        set_top_level_key(&indexed, &missing, KEY, "90"),
        Err(PatchError::DirNotIndexed)
    );
    assert!(!std::path::Path::new(&missing).exists());
}

#[test]
fn a_file_changed_between_the_read_and_the_write_is_not_overwritten() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(settings_of(&dir), "{\"cleanupPeriodDays\":30}").expect("write");
    let indexed = [dir_string(&dir)];

    let result = set_with(
        &indexed,
        &dir_string(&dir),
        KEY,
        "90",
        |path| std::fs::write(path, "{\"model\":\"opus\"}").expect("claude code writes"),
        write_file,
    );
    assert_eq!(result, Err(PatchError::ChangedUnderneath));
    assert_eq!(
        std::fs::read(settings_of(&dir)).expect("read"),
        b"{\"model\":\"opus\"}"
    );

    // A file that appears where there was none counts as a change too.
    let empty = tempfile::tempdir().expect("temp dir");
    let indexed = [dir_string(&empty)];
    let result = set_with(
        &indexed,
        &dir_string(&empty),
        KEY,
        "90",
        |path| std::fs::write(path, "{}").expect("claude code writes"),
        write_file,
    );
    assert_eq!(result, Err(PatchError::ChangedUnderneath));
    assert_eq!(std::fs::read(settings_of(&empty)).expect("read"), b"{}");
}

/// The write is #668's atomic replace: a failure halfway through leaves the
/// previous file byte-identical, and no temp file behind.
#[test]
fn a_failed_write_leaves_the_previous_file_intact() {
    use std::io::Write;

    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(settings_of(&dir), awkward()).expect("write");
    let indexed = [dir_string(&dir)];

    let result = set_with(
        &indexed,
        &dir_string(&dir),
        KEY,
        "90",
        |_| {},
        |path, data| {
            super::super::replace_file(path, |file| {
                file.write_all(&data[..data.len() / 2])?;
                Err(io::Error::other("disk full"))
            })
        },
    );
    assert_eq!(result, Err(PatchError::Io("disk full".to_string())));
    assert_eq!(
        std::fs::read(settings_of(&dir)).expect("read"),
        awkward().into_bytes()
    );
    assert_eq!(std::fs::read_dir(dir.path()).expect("list").count(), 1);
}

/// `write_file` writes through a symlink (#668), so a dotfiles-managed
/// `settings.json` keeps its link and the edit lands in the linked file.
#[cfg(unix)]
#[test]
fn a_symlinked_settings_file_is_edited_through_the_link() {
    let dir = tempfile::tempdir().expect("temp dir");
    let dotfiles = tempfile::tempdir().expect("temp dir");
    let real = dotfiles.path().join("claude.json");
    std::fs::write(&real, "{\"cleanupPeriodDays\":30}").expect("write");
    std::os::unix::fs::symlink(&real, settings_of(&dir)).expect("symlink");
    let indexed = [dir_string(&dir)];

    set_top_level_key(&indexed, &dir_string(&dir), KEY, "90").expect("write");

    assert!(std::fs::symlink_metadata(settings_of(&dir))
        .expect("lstat")
        .file_type()
        .is_symlink());
    assert_eq!(
        std::fs::read(&real).expect("read"),
        b"{\"cleanupPeriodDays\":90}"
    );
}
