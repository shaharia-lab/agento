//! A trigger rule's execution settings, driven all the way to a real
//! subprocess (#565).
//!
//! The chain under test is a stored `trigger_rules` row → `load_rules` →
//! `ExecutionSettings` → `headless_spec` → `build_options` →
//! `Options::build_args` → a command line and a working directory. Every unit
//! test in `dispatcher.rs` can see the first two links and none of them can see
//! the last: **the working directory is not on the command line at all** — it is
//! `Command::current_dir`, so only the process itself can report it. So the
//! fake CLI records `os.getcwd()` alongside its argv, and that recording is the
//! assertion.
//!
//! It inherits `scheduled_run.rs`'s trap — the fake CLI never exits after the
//! result — so a run that went through `claude::client::Session` rather than the
//! one-shot `query` would hang here rather than fail. Each test wraps its await
//! in a deadline that names it.

use std::path::{Path, PathBuf};

use agento_lib::native::trigger::dispatcher;

/// `AGENTO_CLAUDE_EXECUTABLE` is process-wide, so the tests that set it are
/// serialized against each other.
fn env_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn python3() -> Option<String> {
    for candidate in ["python3", "python"] {
        if std::process::Command::new(candidate)
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
        {
            return Some(candidate.to_string());
        }
    }
    None
}

fn json_str(path: &Path) -> String {
    serde_json::to_string(&path.to_string_lossy()).expect("encode path")
}

fn spawn_log(dir: &Path) -> PathBuf {
    dir.join("spawns.jsonl")
}

/// A CLI that records **its own argv and working directory**, acknowledges
/// `initialize`, and answers the first user message.
///
/// `headless_resume.rs` records argv only, which is enough for a `--resume`
/// flag. A working directory has no flag, so this one adds `os.getcwd()`.
fn fake_cli(dir: &Path) -> PathBuf {
    let script = format!(
        r#"#!/usr/bin/env {python}
import json, os, sys

with open({spawn_log}, "a") as _log:
    _log.write(json.dumps({{"argv": sys.argv, "cwd": os.getcwd()}}) + "\n")
    _log.flush()

def say(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

def ack(request_id):
    say({{"type": "control_response",
         "response": {{"subtype": "success", "request_id": request_id,
                       "response": {{"models": [{{"value": "fake", "displayName": "Fake"}}],
                                     "account": {{"apiProvider": "fake"}},
                                     "output_style": "default"}}}}}})

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except Exception:
        continue
    req = msg.get("request") or {{}}
    if msg.get("type") == "control_request" and req.get("subtype") == "initialize":
        ack(req.get("request_id") or msg.get("request_id"))
        continue
    if msg.get("type") == "user":
        say({{"type": "assistant", "session_id": "s-1",
             "message": {{"role": "assistant",
                          "content": [{{"type": "text", "text": "done"}}]}}}})
        say({{"type": "result", "subtype": "success", "session_id": "s-1",
             "is_error": False, "result": "done",
             "usage": {{"input_tokens": 1, "output_tokens": 1}}}})
        # **Deliberately no exit**, as `scheduled_run.rs` explains.
        continue
"#,
        python = python3().unwrap_or_else(|| "python3".into()),
        spawn_log = json_str(&spawn_log(dir)),
    );
    let path = dir.join("fake-claude");
    std::fs::write(&path, script).expect("write fake CLI");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake CLI");
    }
    path
}

/// What the fake CLI recorded about the one time it was spawned.
fn only_spawn(dir: &Path) -> (Vec<String>, String) {
    let raw = std::fs::read_to_string(spawn_log(dir)).expect("the CLI was never spawned");
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "exactly one spawn");
    let entry: serde_json::Value = serde_json::from_str(lines[0]).expect("spawn line");
    (
        serde_json::from_value(entry["argv"].clone()).expect("argv"),
        entry["cwd"].as_str().expect("cwd").to_string(),
    )
}

/// The value that follows `flag` on a command line, or `None`.
fn flag_value<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    let at = argv.iter().position(|a| a == flag)?;
    argv.get(at + 1).map(String::as_str)
}

/// A migrated database with a Telegram integration and one rule carrying the
/// five execution settings.
#[allow(clippy::too_many_arguments)]
fn migrated_with_rule(
    path: &Path,
    model: &str,
    working_directory: &str,
    permission_mode: &str,
    timeout_minutes: i64,
) {
    let mut conn = rusqlite::Connection::open(path).expect("open");
    agento_lib::native::migrate::apply(&mut conn).expect("migrate");
    conn.execute(
        "INSERT INTO integrations (id, name, type, enabled, credentials, services,
                                   created_at, updated_at)
         VALUES ('tg', 'T', 'telegram', 1, '{}', '{}',
                 '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
        [],
    )
    .expect("seed integration");
    conn.execute(
        "INSERT INTO trigger_rules
            (id, integration_id, name, agent_slug, enabled, filter_prefix,
             filter_keywords, filter_chat_ids, model, working_directory,
             settings_profile_id, permission_mode, timeout_minutes,
             created_at, updated_at)
         VALUES ('r', 'tg', 'r', 'a', 1, '', '[]', '[]', ?1, ?2, '', ?3, ?4,
                 '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
        rusqlite::params![model, working_directory, permission_mode, timeout_minutes],
    )
    .expect("seed rule");
}

/// The agent the dispatcher would resolve for a rule that names one — built here
/// rather than seeded, because `resolve_agent` is the dispatcher's business and
/// what this file is about is what happens *after* it.
fn agent() -> agento_lib::native::agents::Agent {
    agento_lib::native::agents::Agent {
        name: "A".to_string(),
        slug: "a".to_string(),
        description: String::new(),
        model: "agent-model".to_string(),
        thinking: String::new(),
        permission_mode: String::new(),
        system_prompt: String::new(),
        capabilities: Default::default(),
        claude_config_dir: String::new(),
    }
}

/// Everything the dispatcher does between a matched rule and the subprocess.
async fn run_the_rule(db: &Path) -> Result<agento_lib::native::agent_run::RunResult, String> {
    let rules = dispatcher::load_rules(db, "tg").expect("load rules");
    let rule = rules.first().expect("one rule");
    let spec = agento_lib::native::agent_run::headless_spec(db, agent(), &rule.settings);
    agento_lib::native::agent_run::run_headless(&spec, "hello", dispatcher::run_timeout(rule)).await
}

/// A rule's working directory, model and permission mode reach the process.
///
/// Before #565 the dispatcher passed two empty strings and no mode at all, so
/// the CLI ran in whatever directory the app was started from, on the agent's
/// model, with permissions bypassed.
#[tokio::test]
async fn a_rules_execution_settings_reach_the_spawned_cli() {
    let Some(_) = python3() else {
        eprintln!("no python3; skipping");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().join("repo");
    std::fs::create_dir(&cwd).expect("mkdir repo");
    // The directory a process reports may be a symlink resolution of the one it
    // was given (`/tmp` is `/private/tmp` on macOS), so compare against the
    // canonical form rather than the string handed to the rule.
    let canonical = std::fs::canonicalize(&cwd).expect("canonicalize");

    let db = dir.path().join("agento.db");
    migrated_with_rule(
        &db,
        "rule-model",
        cwd.to_str().expect("utf-8 path"),
        "plan",
        0,
    );
    let cli = fake_cli(dir.path());

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), run_the_rule(&db))
        .await
        .expect("the run must finish, not hang: a `Session` here never sees stdout close")
        .expect("the run answered");
    assert_eq!(result.answer, "done");

    let (argv, reported_cwd) = only_spawn(dir.path());
    assert_eq!(
        Path::new(&reported_cwd),
        canonical,
        "the rule's working_directory is the process's cwd, and nothing else can show it"
    );
    assert_eq!(
        flag_value(&argv, "--model"),
        Some("rule-model"),
        "the rule's model beats the agent's own"
    );
    assert_eq!(
        flag_value(&argv, "--permission-mode"),
        Some("plan"),
        "and its permission mode reaches build_options rather than the bypassing catch-all"
    );
    // `--allow-dangerously-skip-permissions` is on this line too, and that is
    // not this change: `Options`' own default sets the flag, and only
    // `with_default_permissions()` clears it — so `plan` and `dontAsk` have
    // always carried it, for a chat as much as for a trigger run. What `plan`
    // buys is the mode the CLI reads, which is the flag asserted above.
}

/// A rule that records nothing runs exactly as a trigger run did before #565:
/// the agent's model, no working directory, and the bypassing catch-all that a
/// headless run with no configured mode has always taken.
#[tokio::test]
async fn a_rule_that_configures_nothing_runs_as_it_always_did() {
    let Some(_) = python3() else {
        eprintln!("no python3; skipping");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("agento.db");
    migrated_with_rule(&db, "", "", "", 0);
    let cli = fake_cli(dir.path());

    let _env = env_lock().lock().await;
    std::env::set_var("AGENTO_CLAUDE_EXECUTABLE", &cli);
    tokio::time::timeout(std::time::Duration::from_secs(30), run_the_rule(&db))
        .await
        .expect("the run must finish, not hang")
        .expect("the run answered");

    let (argv, reported_cwd) = only_spawn(dir.path());
    assert_eq!(
        Path::new(&reported_cwd),
        std::fs::canonicalize(std::env::current_dir().expect("cwd")).expect("canonicalize"),
        "an unset working directory leaves the process where the app is"
    );
    assert_eq!(flag_value(&argv, "--model"), Some("agent-model"));
    assert_eq!(
        flag_value(&argv, "--permission-mode"),
        Some("bypassPermissions"),
        "no mode on the rule and none on the agent is `build_options`' catch-all"
    );
}
