use super::*;
use crate::native::tasks::{create_task, get_task, list_tasks};
use axum::http::StatusCode;

fn migrated() -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().expect("temp file");
    let mut conn = rusqlite::Connection::open(file.path()).expect("open");
    crate::native::migrate::apply(&mut conn).expect("migrate");
    file
}

/// A Slack integration holding a bot token, and a Telegram one.
fn migrated_with_integrations(bot_token: &str) -> tempfile::NamedTempFile {
    let file = migrated();
    let conn = rusqlite::Connection::open(file.path()).expect("open");
    conn.execute(
        "INSERT INTO integrations (id, name, type, enabled, credentials, created_at, updated_at)
         VALUES ('slack-int-7f3a', 'Team Slack', 'slack', 1, ?1,
                 '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC'),
                ('tg-int-9c2e', 'Bot', 'telegram', 1, '{}',
                 '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
        [format!(
            r#"{{"auth_mode":"bot_token","bot_token":"{bot_token}"}}"#
        )],
    )
    .expect("seed integrations");
    file
}

fn created(file: &tempfile::NamedTempFile, body: &str) -> ScheduledTask {
    let answer = create_task(file.path(), body.as_bytes()).expect("create");
    assert_eq!(answer.status, StatusCode::CREATED);
    let task: serde_json::Value =
        serde_json::from_slice(answer.body.as_deref().expect("body")).expect("json");
    get_task(file.path(), task["id"].as_str().expect("id"))
        .expect("get")
        .expect("task")
}

fn exported(file: &tempfile::NamedTempFile, id: &str) -> String {
    let recipe = export(file.path(), id).expect("export").expect("recipe");
    String::from_utf8(encode(&recipe).expect("encode")).expect("utf-8")
}

fn task_count(file: &tempfile::NamedTempFile) -> usize {
    list_tasks(file.path()).expect("list").len()
}

/// A key of Anthropic's shape, built from pieces so no literal is a secret.
fn pasted_anthropic_key() -> String {
    format!(
        "sk-ant-{}{}-{}",
        "api", "03", "Zq9xWb7Lm2Kp4Rt6Yv8Nc1Hd3Fg5Js0Ae"
    )
}

/// Every place a secret or credential id can sit on a task, seeded, and none
/// of it in the exported bytes.
#[test]
fn an_export_carries_no_secret_from_any_place_one_can_sit() {
    let bot_token = format!("xoxb-{}-{}", "2718281828", "S3cr3tB0tT0k3nV4lue");
    let file = migrated_with_integrations(&bot_token);
    let pasted = pasted_anthropic_key();
    let body = format!(
        r#"{{"name":"Weekly digest","description":"for the team","prompt":"summarise with {pasted} please",
        "agent_slug":"writer","working_directory":"/home/alice/secret-project",
        "settings_profile_id":"profile-secret-77","permission_mode":"read_only",
        "schedule_type":"cron","schedule_config":{{"expression":"0 9 * * 1"}},
        "destinations":[
          {{"type":"slack","when":"success","slack":{{"integration_id":"slack-int-7f3a","channel_ids":["C0SECRET01"]}}}},
          {{"type":"telegram","when":"always","telegram":{{"integration_id":"tg-int-9c2e","chat_ids":["-1009876543210"]}}}},
          {{"type":"email","when":"success","email":{{"recipients":["boss.secret@example.com"]}}}},
          {{"type":"reply","when":"always"}}]}}"#
    );
    let task = created(&file, &body);

    let conn = rusqlite::Connection::open(file.path()).expect("open");
    conn.execute(
        r#"INSERT INTO trigger_rules (id, integration_id, name, agent_slug, enabled,
               filter_prefix, filter_keywords, filter_chat_ids, filter_user_ids,
               model, working_directory, settings_profile_id, task_id, created_at, updated_at)
           VALUES ('rule-secret-1', 'tg-int-9c2e', 'digest', 'writer', 1,
               '/digest', '["weekly"]', '["5550001234"]', '["U0SECRETUSR"]',
               'claude-opus-4-1', '/home/alice/rule-dir', 'rule-profile-secret', ?1,
               '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')"#,
        [&task.id],
    )
    .expect("seed rule");
    let keypair = crate::native::security::keys::Keypair::generate().expect("keypair");
    let hook =
        crate::native::security::token::mint_hook(&keypair, "s", &task.id, 600).expect("mint hook");
    crate::native::security::tokens::insert(
        &conn,
        "hook-token-row-1",
        "digest hook",
        crate::native::security::token::Scope::Hook,
        &task.id,
        &hook.jti,
        "2026-01-01 00:00:00 +0000 UTC",
        "2027-01-01 00:00:00 +0000 UTC",
    )
    .expect("seed hook token");

    let recipe = exported(&file, &task.id);
    for (what, secret) in [
        ("bot token", bot_token.as_str()),
        ("slack integration id", "slack-int-7f3a"),
        ("telegram integration id", "tg-int-9c2e"),
        ("slack channel", "C0SECRET01"),
        ("telegram chat", "-1009876543210"),
        ("email recipient", "boss.secret@example.com"),
        ("rule chat allow-list", "5550001234"),
        ("rule user allow-list", "U0SECRETUSR"),
        ("rule id", "rule-secret-1"),
        ("rule working directory", "/home/alice/rule-dir"),
        ("rule settings profile", "rule-profile-secret"),
        ("rule model", "claude-opus-4-1"),
        ("task id", task.id.as_str()),
        ("working directory", "/home/alice"),
        ("settings profile", "profile-secret-77"),
        ("hook token", hook.token.as_str()),
        ("hook jti", hook.jti.as_str()),
        ("hook token row", "hook-token-row-1"),
        ("prompt key", pasted.as_str()),
    ] {
        assert!(!recipe.contains(secret), "the export carries the {what}");
    }
    let masked = mask_text(&pasted);
    assert!(masked != pasted, "the Credentials Checker masks the key");
    assert!(
        recipe.contains(&format!(r#""prompt":"summarise with {masked} please""#)),
        "the prompt is kept around its masked key"
    );
    // The wiring is still described, by kind only.
    assert!(recipe.ends_with(
        r#""destinations":[{"type":"slack","when":"success"},{"type":"telegram","when":"always"},{"type":"email","when":"success"},{"type":"reply","when":"always"}],"triggers":[{"type":"telegram","filter_prefix":"/digest","filter_keywords":["weekly"]}]}
"#
    ));
}

/// The full key order of a version-1 recipe, pinned as bytes.
const GOLDEN: &str = r#"{"agento_recipe":1,"name":"Weekly \u003cdigest\u003e","description":"for the team","prompt":"summarise the week","agent_slug":"writer","model":"claude-sonnet-4-5","timeout_minutes":45,"schedule_type":"cron","schedule_config":{"expression":"0 9 * * 1"},"save_output":true,"continue_on_reply":true,"max_concurrent_runs":2,"max_queued_events":3,"max_runs_per_hour":4,"permission_mode":"read_only","destinations":[{"type":"slack","when":"success"},{"type":"reply","when":"always"}],"triggers":[{"type":"slack","filter_prefix":"","filter_keywords":[]},{"type":"telegram","filter_prefix":"/digest","filter_keywords":["weekly"]}]}
"#;

/// The golden imports, and exporting the import gives the golden back, less
/// the destinations and triggers import does not create.
#[test]
fn export_import_export_round_trips_the_golden_bytes() {
    let file = migrated_with_integrations("x");
    let source = created(
        &file,
        r#"{"name":"Weekly <digest>","description":"for the team","prompt":"summarise the week",
        "agent_slug":"writer","model":"claude-sonnet-4-5","timeout_minutes":45,
        "schedule_type":"cron","schedule_config":{"expression":"0 9 * * 1"},"save_output":true,
        "continue_on_reply":true,"max_concurrent_runs":2,"max_queued_events":3,"max_runs_per_hour":4,
        "permission_mode":"read_only","destinations":[
          {"type":"slack","slack":{"integration_id":"slack-int-7f3a","channel_ids":["C0123ABCD"]}},
          {"type":"reply","when":"always"}]}"#,
    );
    let conn = rusqlite::Connection::open(file.path()).expect("open");
    conn.execute_batch(&format!(
        r#"INSERT INTO trigger_rules (id, integration_id, agent_slug, filter_prefix,
               filter_keywords, task_id, created_at, updated_at) VALUES
           ('r2', 'tg-int-9c2e', 'writer', '/digest', '["weekly"]', '{id}',
            '2026-01-02 00:00:00 +0000 UTC', '2026-01-02 00:00:00 +0000 UTC'),
           ('r1', 'slack-int-7f3a', 'writer', '', '[]', '{id}',
            '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC'),
           ('r3', 'slack-int-7f3a', 'writer', '', '[]', 'another-task',
            '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC');"#,
        id = source.id
    ))
    .expect("seed rules");

    let first = exported(&file, &source.id);
    assert_eq!(first, GOLDEN);

    let answer = import(file.path(), first.as_bytes()).expect("import");
    assert_eq!(answer.status, StatusCode::CREATED);
    let imported: serde_json::Value =
        serde_json::from_slice(answer.body.as_deref().expect("body")).expect("json");
    let second = exported(&file, imported["id"].as_str().expect("id"));
    let without_wiring = GOLDEN.replace(
        r#""destinations":[{"type":"slack","when":"success"},{"type":"reply","when":"always"}],"triggers":[{"type":"slack","filter_prefix":"","filter_keywords":[]},{"type":"telegram","filter_prefix":"/digest","filter_keywords":["weekly"]}]}"#,
        r#""destinations":[{"type":"reply","when":"always"}]}"#,
    );
    assert_eq!(second, without_wiring);
}

/// An active source imports paused, the response is `POST /api/tasks`'s
/// encoding, and a scheduler reconciling the database registers no timer.
#[tokio::test]
async fn an_import_is_a_201_paused_task_the_scheduler_does_not_register() {
    let file = migrated();
    let source = created(
        &file,
        r#"{"name":"N","prompt":"p","permission_mode":"full","schedule_type":"interval",
            "schedule_config":{"every_hours":1}}"#,
    );
    assert_eq!(source.status, "active");

    let answer = import(file.path(), exported(&file, &source.id).as_bytes()).expect("import");
    assert_eq!(answer.status, StatusCode::CREATED);
    let body: serde_json::Value =
        serde_json::from_slice(answer.body.as_deref().expect("body")).expect("json");
    let id = body["id"].as_str().expect("id");
    assert_ne!(id, source.id);
    assert_eq!(body["status"], "paused");
    let stored = get_task(file.path(), id).expect("get").expect("task");
    assert_eq!(stored.status, "paused");
    assert_eq!(
        String::from_utf8(answer.body.expect("body")).expect("utf-8"),
        String::from_utf8(crate::native::gojson::to_vec(&stored).expect("encode")).expect("utf-8"),
        "the answer is the stored task in the task encoding"
    );

    let scheduler = crate::native::schedule::runtime::detached(file.path());
    scheduler.reconcile();
    assert!(
        scheduler.knows_task(&source.id),
        "the active source is scheduled"
    );
    assert!(!scheduler.knows_task(id), "the imported task is not");
}

fn refused(file: &tempfile::NamedTempFile, body: &str) -> WriteError {
    let before = task_count(file);
    let err = import(file.path(), body.as_bytes()).expect_err(body);
    assert_eq!(
        task_count(file),
        before,
        "a refused import writes nothing: {body}"
    );
    err
}

fn assert_422(file: &tempfile::NamedTempFile, body: &str, field: &str) {
    let err = refused(file, body);
    assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY, "for {body}");
    assert!(
        err.message()
            .starts_with(&format!("validation error for \"{field}\":")),
        "for {body}: {}",
        err.message()
    );
}

const VALID: &str = r#""name":"N","prompt":"p","permission_mode":"plan""#;

#[test]
fn a_missing_or_unsupported_version_is_a_422_naming_agento_recipe() {
    let file = migrated();
    for version in [
        "",
        r#""agento_recipe":2,"#,
        r#""agento_recipe":"1","#,
        r#""agento_recipe":1.0,"#,
        r#""agento_recipe":null,"#,
        r#""agento_recipe":0,"#,
    ] {
        assert_422(&file, &format!("{{{version}{VALID}}}"), "agento_recipe");
    }
}

#[test]
fn an_unknown_key_at_any_level_is_a_422_naming_it() {
    let file = migrated();
    for (extra, field) in [
        (r#","status":"active""#, "status"),
        (r#","id":"abc""#, "id"),
        (r#","working_directory":"/w""#, "working_directory"),
        (
            r#","destinations":[{"type":"reply"},{"type":"slack","integration_id":"s"}]"#,
            "destinations[1].integration_id",
        ),
        (
            r#","triggers":[{"type":"telegram","filter_chat_ids":["1"]}]"#,
            "triggers[0].filter_chat_ids",
        ),
    ] {
        assert_422(
            &file,
            &format!(r#"{{"agento_recipe":1,{VALID}{extra}}}"#),
            field,
        );
    }
}

#[test]
fn a_body_that_is_not_a_json_object_is_a_400() {
    let file = migrated();
    for body in ["", "not json", "[]", r#""recipe""#, "1"] {
        assert_eq!(
            refused(&file, body).status(),
            StatusCode::BAD_REQUEST,
            "for {body:?}"
        );
    }
    // A known key of the wrong shape is the typed decode's 400.
    assert_eq!(
        refused(
            &file,
            &format!(r#"{{"agento_recipe":1,{VALID},"destinations":{{}}}}"#)
        )
        .status(),
        StatusCode::BAD_REQUEST
    );
}

#[test]
fn the_task_rules_and_the_descriptive_entries_are_validated() {
    let file = migrated();
    for (body, field) in [
        (
            r#"{"agento_recipe":1,"name":"N","prompt":"p"}"#,
            "permission_mode",
        ),
        (
            r#"{"agento_recipe":1,"name":"N","prompt":"p","permission_mode":"root"}"#,
            "permission_mode",
        ),
        (
            r#"{"agento_recipe":1,"prompt":"p","permission_mode":"plan"}"#,
            "name",
        ),
        (
            r#"{"agento_recipe":1,"name":"N","prompt":"p","permission_mode":"plan","destinations":[{"type":"fax"}]}"#,
            "destinations[0].type",
        ),
        (
            r#"{"agento_recipe":1,"name":"N","prompt":"p","permission_mode":"plan","destinations":[{"type":"email","when":"never"}]}"#,
            "destinations[0].when",
        ),
        (
            r#"{"agento_recipe":1,"name":"N","prompt":"p","permission_mode":"plan","destinations":[{"type":"reply"},{"type":"reply"}]}"#,
            "destinations[1].type",
        ),
        (
            r#"{"agento_recipe":1,"name":"N","prompt":"p","permission_mode":"plan","triggers":[{"type":"webhook"}]}"#,
            "triggers[0].type",
        ),
    ] {
        assert_422(&file, body, field);
    }
}

#[test]
fn only_a_reply_destination_is_created_and_no_trigger_rule_is() {
    let file = migrated();
    let answer = import(
        file.path(),
        br#"{"agento_recipe":1,"name":"N","prompt":"p","permission_mode":"full",
            "destinations":[{"type":"slack","when":"success"},{"type":"telegram","when":"always"},
                            {"type":"email","when":"success"},{"type":"reply","when":"always"}],
            "triggers":[{"type":"slack","filter_prefix":"","filter_keywords":null}]}"#,
    )
    .expect("import");
    let body: serde_json::Value =
        serde_json::from_slice(answer.body.as_deref().expect("body")).expect("json");
    let task = get_task(file.path(), body["id"].as_str().expect("id"))
        .expect("get")
        .expect("task");
    assert_eq!(
        task.destinations,
        vec![TaskDestination {
            r#type: "reply".to_string(),
            when: "always".to_string(),
            ..Default::default()
        }]
    );
    assert_eq!(
        task.permission_mode, "full",
        "the recipe's mode is imported as stated"
    );
    let conn = rusqlite::Connection::open(file.path()).expect("open");
    let rules: i64 = conn
        .query_row("SELECT COUNT(*) FROM trigger_rules", [], |r| r.get(0))
        .expect("count");
    assert_eq!(rules, 0);
}

#[test]
fn the_recipe_of_an_unknown_task_is_a_404() {
    let file = migrated();
    assert!(export(file.path(), "nope").expect("export").is_none());
    let err = answer(file.path(), "nope").expect_err("404");
    assert_eq!(err.status(), StatusCode::NOT_FOUND);
}

/// A weekly digest (#699) round-trips through a recipe: the export carries
/// `builtin` whole, after `permission_mode`, and the import stores it, empty
/// prompt and all. A task with no built-in exports no `builtin` key.
#[test]
fn a_weekly_digest_round_trips_its_builtin() {
    let file = migrated();
    let digest = created(
        &file,
        r#"{"name":"Digest","permission_mode":"read_only",
            "builtin":{"kind":"weekly_digest","narrative":true}}"#,
    );
    let recipe = exported(&file, &digest.id);
    assert!(
        recipe.contains(
            r#""permission_mode":"read_only","builtin":{"kind":"weekly_digest","narrative":true}}"#
        ),
        "{recipe}"
    );

    let answer = import(file.path(), recipe.as_bytes()).expect("import");
    assert_eq!(answer.status, StatusCode::CREATED);
    let task: serde_json::Value =
        serde_json::from_slice(answer.body.as_deref().expect("body")).expect("json");
    let imported = get_task(file.path(), task["id"].as_str().expect("id"))
        .expect("get")
        .expect("task");
    assert_eq!(imported.builtin, digest.builtin);
    assert_eq!(imported.prompt, "");
    assert_eq!(imported.status, "paused");

    let plain = created(&file, &format!("{{{VALID}}}"));
    assert!(!exported(&file, &plain.id).contains("builtin"));
}

/// An unknown kind in a recipe is the same 422 the task write answers, and an
/// unknown key inside `builtin` is a 422 naming it, as at every other level.
#[test]
fn a_recipe_with_an_unknown_builtin_kind_or_key_is_a_422() {
    let file = migrated();
    assert_422(
        &file,
        &format!(r#"{{"agento_recipe":1,{VALID},"builtin":{{"kind":"monthly"}}}}"#),
        "builtin",
    );
    assert_422(
        &file,
        &format!(
            r#"{{"agento_recipe":1,{VALID},"builtin":{{"kind":"weekly_digest","narative":true}}}}"#
        ),
        "builtin.narative",
    );
}
