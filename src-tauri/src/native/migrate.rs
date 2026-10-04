//! The schema: what Rust knows about it, and what it is allowed to do to it.
//!
//! # The migrations are not transcribed
//!
//! `MIGRATIONS` is parsed from `desktop/parity/migrations_vectors.json`, which
//! is **generated from Go** (`go test ./internal/storage/
//! -update-migration-vectors`) and asserted against the `migrations` slice by
//! `internal/storage/migrations_vector_test.go`. Adding migration 28 without
//! regenerating fails Go's own test suite.
//!
//! # ...but migration 31 onward is this build's own (#405, #422, #433, #425)
//!
//! That paragraph describes migrations 1–30, and they are still exactly Go's
//! bytes. It stopped being the whole story with #405, which needs an
//! `api_tokens` table `main`'s server has never heard of. There is no generator
//! left to run — #391 deleted the Go tree — so migration 31 is **authored**
//! here, and the vector file's own `_comment` says so at the top.
//!
//! Migration **32** is the second of them: the LLM gateway's three
//! configuration tables (#422, epic #421). Migration **33** is the third: the
//! `session_search` FTS5 index and `claude_cache_metadata.search_index_version`
//! (#433, epic #432). Migration **34** is the fourth: `gateway_usage_log`
//! (#425, epic #421). Migration **35** is the fifth: the gateway's usage
//! retention horizon (#428, epic #421). Migration **36** is the sixth:
//! `session_insights.search_index_version` and the `session_search_key` rowid
//! side table (#446, epic #432) — which is also what leaves 33's
//! `claude_cache_metadata.search_index_version` a dead column, kept because the
//! migrations are append-only. Migration **37** is the seventh: the three
//! `chat_sessions.continued_from_*` columns that record what a chat resumes and
//! where its inherited history ends (#490). Migration **38** is the eighth:
//! `user_settings.claude_executable_path`, the in-product escape hatch for when
//! Claude Code CLI detection cannot find an install (#503). Migration **39** is
//! the ninth: the five execution settings a trigger rule may now carry
//! (`model`, `working_directory`, `settings_profile_id`, `permission_mode`,
//! `timeout_minutes`), the `inbound_*` state columns on `integrations`, and the
//! `inbound_threads` / `slack_processed_events` tables the Slack inbound half
//! reads and writes (#563, epic #562). Migration **40** is the tenth:
//! `job_history.pid` and `job_history.pid_started_at`, the OS process a run
//! spawned and when, so something outside the run can find it (#594, epic #593).
//! Migration **41** is the eleventh: the Credentials Checker's
//! `credential_findings`, `credential_whitelist` and `credential_scan_state`
//! tables, none of which may ever hold a raw secret (#600, epic #597).
//! Migration **42** is the twelfth: `user_settings.credentials_checker_enabled`,
//! the Credentials Checker's on/off switch, default off (#601, epic #597).
//! Migration **43** is the thirteenth: `credential_findings.match_hash` and its
//! index, so a whitelist entry added by value can suppress existing findings of
//! that value retroactively (#602, epic #597). Migration **44** is the
//! fourteenth: `scheduled_tasks.destinations`, the JSON list of where a task's
//! output is delivered after a run (#634, epic #626). Migration **45** is the
//! fifteenth: the `job_deliveries` table, one row per channel a run's output
//! was delivered to and how that went, kept off the run's own row (#635, epic
//! #626). Migration **46** is the sixteenth: `transcript_expired_at` and
//! `harness` on both session cache tables, and the single-row
//! `install_identity` table whose random `machine_id` the migration itself
//! generates, once per database (#704, epic #703).
//! Migration **47** is the seventeenth:
//! `user_settings.session_history_retention_days`, how long an expired
//! session's history is kept, where 0 is for ever (#712, epic #703).
//! Migration **48** is the eighteenth: the automations data model —
//! `job_history.triggered_by`, `continues_job_id` and `event_payload`,
//! `trigger_rules.task_id`, `continue_on_reply` on both `trigger_rules` and
//! `scheduled_tasks`, and the two per-task event counters. It also backfills:
//! every existing rule and every task with a Slack destination is switched to
//! `continue_on_reply`, because those already continue on a reply (#681, epic
//! #679). Migration **49** is the nineteenth:
//! `user_settings.claude_retention_prompt_answered`, whether the one-time
//! retention prompt has been answered, default no (#720, epic #703).
//! Migration **50** is the twentieth and adds no column: it turns
//! `inbound_enabled` on for every Telegram row whose webhook was `active`, so a
//! trigger that worked over the webhook keeps working over the long poll that
//! replaces it (#676).
//! Migration **51** is the twenty-first and adds no column either: it turns
//! off every enabled Telegram trigger rule whose `filter_chat_ids` names no
//! chat, because such a rule now answers nobody and the write refuses one
//! (#674). The rule and its settings are kept; listing a chat turns it back on.
//! Migration **52** is the twenty-second: `job_history.machine_id` and
//! `harness`, which install ran a run and on what. Existing rows are backfilled
//! with this install's `install_identity.machine_id` and `claude` (#678).
//! Migration **53** is the twenty-third: `trigger_rules.filter_user_ids`, the
//! users a Slack rule answers, default `[]`. No row is rewritten and `enabled`
//! is not touched, so an existing Slack rule keeps its switch and answers
//! nobody until a user is listed (#688).
//! Migration **54** is the twenty-fourth: `scheduled_tasks.max_concurrent_runs`,
//! `max_queued_events` and `max_runs_per_hour`, the per-task limits on event
//! runs, defaulting to 1, 5 and 10 (#691).
//! Migration **55** is the twenty-fifth and adds no column: it rewrites the
//! `database-url-credentials` snippets stored under ruleset 1 whose tail held
//! the `@` and the end of the password, because a rescan never reaches an
//! expired session's rows (#741).
//! Same terms every time — authored,
//! additive, and
//! appended to the vector file as *text*, because a JSON round-trip through most
//! encoders rewrites Go's `>` escaping across the frozen entries and that is the
//! reformat the file's `_comment` forbids.
//!
//! **Two authors will reach for the same number.** #422 and #433 were worked in
//! parallel and both wrote migration 32; the second to merge has to rebase and
//! renumber, and the only thing that catches it is the hardcoded count below
//! disagreeing with the file. Re-read the vector file's tail after any rebase
//! that touches it rather than trusting a clean auto-merge — appending to a JSON
//! array from two branches conflicts, but appending a *sibling* object does not
//! have to.
//!
//! It happened again on #425, and the second time was cheaper only because the
//! trap was written down: that issue's refinement recorded "32 is the latest, so
//! this is 33", which was true when it was written and false by the time it was
//! implemented — #433 had merged in between. **A migration number in an issue
//! body is a snapshot, not an instruction.** Re-derive it from this file's tail
//! and check `gh pr list` for an in-flight PR that appends one.
//!
//! Two rules follow, and both are asserted below:
//!
//! - **Do not edit 1–30.** They are a frozen record of what Go produced, and
//!   the only thing that still makes the "not transcribed" argument above true.
//! - **Anything appended must be additive.** Migrations run only when *newer*
//!   than the recorded version, so a database at the latest version makes an
//!   older build apply nothing and carry on — it simply never reads the new
//!   table. A migration that *altered* an existing column would break a
//!   downgrade instead, silently, on a user's machine. Downgrades are already
//!   refused below 0.1.1 (see `docs/troubleshooting.md`), but the rule keeps the
//!   damage to a refusal rather than corruption.
//!
//! Twenty-seven migrations of hand-copied DDL is precisely the kind of thing
//! that agrees on every table anyone happens to check and differs on one column
//! default nobody does — and the failure would surface as a write succeeding
//! against a column that is `NOT NULL DEFAULT ''` on one side and nullable on
//! the other. Embedding the file removes the transcription entirely. It also
//! outlives the implementation that generated it: this file **is** the record
//! of what the schema is, the same reason the rest of `parity/` exists.
//!
//! # `verify` and `apply` are separate, and the split is load-bearing
//!
//! [`apply`] runs once at startup, from `lib.rs`, before anything serves.
//! [`verify`] runs on every write, and refuses a database whose version is not
//! the one this build compiled against — in either direction.
//!
//! Keeping them apart matters because the version is read **outside** the
//! transaction that applies the next migration. Two processes starting together
//! would both read the same version, both decide to apply the next migration,
//! and both run its DDL: the loser gets `table already exists`, which is not a
//! conflict it retries but an error that fails startup outright. Exactly one
//! process may migrate a given data directory, which is what
//! `tauri-plugin-single-instance` is for — and why pointing a second instance
//! at the same `AGENTO_DATA_DIR` is documented as unsupported.
//!
//! # Two directions, two different answers
//!
//! A database **older** than this build is a hard error: some migration this
//! code depends on has not run, so a write would hit a missing column. A
//! database **newer** is also an error here: this build's queries are compiled
//! against the schema it ships with, so a newer file has columns it was never
//! compiled against. Both directions return `Err`, which is a 500 — the honest
//! answer, because the alternative is reading or writing a shape this build
//! does not understand.

use std::sync::OnceLock;

use rusqlite::Connection;

/// One migration, exactly as `internal/storage` applies it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Migration {
    pub version: i64,
    pub sql: String,
}

#[derive(serde::Deserialize)]
struct VectorFile {
    migrations: Vec<Migration>,
}

/// The vector file, embedded at compile time so a build can never disagree with
/// the tree it was built from.
const VECTORS: &str = include_str!("../../../parity/migrations_vectors.json");

/// Every migration, in the order Go applies them.
///
/// # Panics
///
/// Only if the embedded vector file is malformed, which is a build-time fact
/// rather than a runtime one: the same bytes are parsed on every run, and the
/// unit tests below parse them too, so a bad file fails `cargo test` and CI
/// long before it can reach a user.
pub fn migrations() -> &'static [Migration] {
    static PARSED: OnceLock<Vec<Migration>> = OnceLock::new();
    PARSED.get_or_init(|| {
        let file: VectorFile = serde_json::from_str(VECTORS)
            .expect("migrations_vectors.json is embedded and must parse");
        file.migrations
    })
}

/// The version this build was written against — the highest it knows.
pub fn expected_version() -> i64 {
    migrations().last().map(|m| m.version).unwrap_or(0)
}

/// The version recorded in the database.
///
/// Mirrors Go's `currentVersion`: `COALESCE(MAX(version), 0)`, so a database
/// whose `schema_migrations` table exists but is empty reads as 0 rather than
/// failing. A database with no such table at all has never been migrated, and
/// that is an error here rather than a 0 — Go creates the table as its first
/// act, so its absence means this is not an Agento database.
pub fn current_version(conn: &Connection) -> Result<i64, String> {
    conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )
    .map_err(|e| format!("reading schema version: {e}"))
}

/// Confirm the database is the schema this build writes against.
///
/// Called by every native write before it touches anything. The cost is one
/// indexed aggregate over a table with one row per migration; the alternative
/// is discovering the mismatch as a constraint violation halfway through a
/// transaction.
pub fn verify(conn: &Connection) -> Result<(), String> {
    let want = expected_version();
    let have = current_version(conn)?;
    if have == want {
        return Ok(());
    }
    // Both directions are a 500, but they are different situations and a log
    // line that says which one saves the next person a bisect.
    if have < want {
        return Err(format!(
            "database is at schema version {have}, this build writes version {want}; \
             migrations have not been applied"
        ));
    }
    Err(format!(
        "database is at schema version {have}, newer than this build's {want}; \
         it was written by a later version of Agento"
    ))
}

/// Apply every pending migration.
///
/// Called once at startup, from `lib.rs`, before the window shows.
///
/// Mirrors `applyMigrations`/`applyMigration`: create the tracking table,
/// read the current version, then run each pending migration and record it.
/// One departure, and it is the reason the Go version cannot be run twice
/// concurrently: the version is re-read **inside** the transaction that applies
/// the next migration, so two processes racing resolve to one applying and the
/// other finding its work already done, instead of one failing on duplicate
/// DDL. `BEGIN IMMEDIATE` takes the write lock up front rather than on first
/// write, which is what makes that re-read authoritative.
pub fn apply(conn: &mut Connection) -> Result<(), String> {
    // `BEGIN IMMEDIATE` takes the write lock up front, so a contended database
    // needs a busy timeout or the loser fails on the lock rather than waiting
    // for it — which would be the same failure this function exists to avoid,
    // one layer down.
    //
    // rusqlite already sets 5000 ms on every `Connection::open`
    // (`InnerConnection::open_with_flags`), so this is **explicitness, not a
    // fix**: it states the value the correctness argument depends on instead of
    // inheriting it from a dependency's default, which a version bump could
    // change without anything here noticing.
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| format!("setting busy_timeout: {e}"))?;

    // Outside any transaction, like Go's. Harmless to race: `IF NOT EXISTS`.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    INTEGER PRIMARY KEY,
            applied_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
        )",
    )
    .map_err(|e| format!("creating schema_migrations table: {e}"))?;

    for migration in migrations() {
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| format!("begin migration {}: {e}", migration.version))?;

        // Re-read under the write lock. Without this, the check and the apply
        // are two steps a second process can interleave.
        let current: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("reading schema version: {e}"))?;
        if migration.version <= current {
            // `tx` is dropped here, and rusqlite's default drop behaviour is
            // rollback — so this releases the write lock rather than leaking it.
            continue;
        }

        tx.execute_batch(&migration.sql)
            .map_err(|e| format!("migration {}: {e}", migration.version))?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            rusqlite::params![migration.version, super::gotime::now_go_text()],
        )
        .map_err(|e| format!("recording migration {}: {e}", migration.version))?;

        tx.commit()
            .map_err(|e| format!("commit migration {}: {e}", migration.version))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded file has to parse, and it has to be the whole schema.
    /// Hardcoded rather than derived, for the same reason `sqlite_test.go`
    /// hardcodes its version: a count computed from the list agrees with itself
    /// no matter what the list lost.
    #[test]
    fn the_embedded_vector_is_the_whole_schema() {
        let all = migrations();
        assert_eq!(all.len(), 55, "expected 55 migrations");
        assert_eq!(expected_version(), 55);
        for (i, m) in all.iter().enumerate() {
            assert_eq!(
                m.version,
                i as i64 + 1,
                "versions must be contiguous from 1"
            );
            assert!(!m.sql.is_empty(), "migration {} has no SQL", m.version);
        }
    }

    /// **The boundary between Go's migrations and this branch's** (#405).
    ///
    /// 1–30 are the frozen record of what `internal/storage` applied and must
    /// never be edited; 31 onward is authored here, because #391 deleted the
    /// generator. Pinned as a number rather than left implicit so that appending
    /// a migration is a deliberate act with a line to change, and so that a
    /// *rewrite* of one of Go's — the thing that would quietly destroy the "not
    /// transcribed" property — shows up as a failing hash rather than as
    /// nothing at all.
    #[test]
    fn the_migrations_go_generated_are_unchanged() {
        const LAST_GO_MIGRATION: i64 = 30;

        let all = migrations();
        let go: Vec<&Migration> = all
            .iter()
            .filter(|m| m.version <= LAST_GO_MIGRATION)
            .collect();
        assert_eq!(go.len(), LAST_GO_MIGRATION as usize);

        // A digest over the whole Go half, so editing any one of them fails
        // here with a message that says which rule was broken. Update this
        // constant only if the Go tree is restored and regenerates the file.
        let mut hasher = ring::digest::Context::new(&ring::digest::SHA256);
        for m in &go {
            hasher.update(m.version.to_string().as_bytes());
            hasher.update(b"\0");
            hasher.update(m.sql.as_bytes());
            hasher.update(b"\0");
        }
        let digest: String = hasher
            .finish()
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            digest, GO_MIGRATIONS_SHA256,
            "migrations 1-{LAST_GO_MIGRATION} are Go's frozen output and must not be \
             edited; append a new version instead"
        );
    }

    /// The digest of migrations 1–30, recorded when #405 appended the first
    /// non-Go migration.
    const GO_MIGRATIONS_SHA256: &str =
        "fb4ae3ab30711f1532444af09913c643a1a662564750fbb81a1b841e333c6da3";

    /// The point of embedding rather than transcribing: the SQL must be Go's,
    /// unreformatted. Spot-check a few things a prettifier would silently
    /// change and a hand-copy would silently drop.
    #[test]
    fn the_sql_is_gos_bytes() {
        let all = migrations();
        assert!(
            all[0].sql.starts_with('\n'),
            "migration 1 keeps its leading newline"
        );
        assert!(all[0]
            .sql
            .contains("model           TEXT NOT NULL DEFAULT 'claude-sonnet-4-6'"));
        // Migration 24's RENAME is the one that makes physical column order
        // differ from declaration order in session_insights.
        assert!(all[23]
            .sql
            .contains("RENAME COLUMN thinking_time_ms TO claude_working_time_ms"));
        // Only 9, 26, 27, 28 and 29 use IF NOT EXISTS; migration 2 must not have
        // acquired one, or a half-applied database would look migrated.
        assert!(!all[1].sql.contains("IF NOT EXISTS"));
    }

    /// Applying the whole list against an empty database must produce a
    /// working schema — which is also the cheapest possible proof that the
    /// embedded SQL is valid SQLite rather than merely well-formed JSON.
    #[test]
    fn applying_every_migration_builds_the_schema() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        conn.execute_batch("PRAGMA foreign_keys=ON")
            .expect("pragma");

        apply(&mut conn).expect("apply");

        assert_eq!(current_version(&conn).expect("version"), 55);
        verify(&conn).expect("verify");

        // A column from the last migration, and the one migration 24 renamed:
        // between them they prove the list ran in order and to the end.
        let cols: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('claude_session_cache') WHERE name = 'config_dir'",
                [],
                |row| row.get(0),
            )
            .expect("config_dir");
        assert_eq!(cols, 1);
        let renamed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('session_insights') WHERE name = 'claude_working_time_ms'",
                [],
                |row| row.get(0),
            )
            .expect("renamed column");
        assert_eq!(renamed, 1);
        let destinations: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('scheduled_tasks') WHERE name = 'destinations'",
                [],
                |row| row.get(0),
            )
            .expect("destinations");
        assert_eq!(destinations, 1);
        let old: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('session_insights') WHERE name = 'thinking_time_ms'",
                [],
                |row| row.get(0),
            )
            .expect("old column");
        assert_eq!(old, 0, "migration 24 renames rather than adding");
    }

    /// **Migration 39's mapping table is a table so that SQL can hold these four
    /// properties** (#563), and asserting them here is what makes that a
    /// property rather than a comment in the DDL.
    ///
    /// One row per Slack thread and one thread per Agento chat are the two
    /// uniqueness rules a JSON blob could only enforce in code; the two cascades
    /// are what stop a deleted chat or a deleted integration leaving a mapping
    /// that resolves to nothing at request time.
    ///
    /// `foreign_keys=ON` is set explicitly because **it is per connection** —
    /// `db::open_read_write` sets it in production, and without it both cascades
    /// silently stop firing and this test still passes its first half.
    #[test]
    fn the_inbound_thread_mapping_is_unique_both_ways_and_cascades() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        apply(&mut conn).expect("apply");
        conn.execute_batch("PRAGMA foreign_keys=ON")
            .expect("pragma");

        conn.execute_batch(
            "INSERT INTO integrations (id, name, type, enabled, created_at, updated_at)
                  VALUES ('int-1', 'Slack', 'slack', 1, '', ''),
                         ('int-2', 'Other', 'slack', 1, '', '');
             INSERT INTO chat_sessions (id, agent_slug, created_at, updated_at)
                  VALUES ('chat-1', 'a', '', ''), ('chat-2', 'a', '', '');
             INSERT INTO inbound_threads
                     (integration_id, channel_id, thread_ts, chat_id)
                  VALUES ('int-1', 'C1', '111.0', 'chat-1');",
        )
        .expect("seed");

        let insert = |integration: &str, channel: &str, ts: &str, chat: &str| {
            conn.execute(
                "INSERT INTO inbound_threads (integration_id, channel_id, thread_ts, chat_id)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![integration, channel, ts, chat],
            )
        };

        // The same thread twice, even pointing at a different chat.
        insert("int-1", "C1", "111.0", "chat-2").expect_err("one row per thread");
        // The same chat twice, even from a different thread.
        insert("int-1", "C2", "222.0", "chat-1").expect_err("one thread per chat");
        // Neither rule is a blanket refusal: a different thread and a different
        // chat is fine, and so is the same channel and timestamp under another
        // integration.
        insert("int-1", "C2", "222.0", "chat-2").expect("a distinct pair");
        conn.execute("DELETE FROM inbound_threads WHERE chat_id = 'chat-2'", [])
            .expect("clean up");
        insert("int-2", "C1", "111.0", "chat-2").expect("scoped to one integration");

        let rows = |conn: &Connection| -> i64 {
            conn.query_row("SELECT COUNT(*) FROM inbound_threads", [], |row| row.get(0))
                .expect("count")
        };
        assert_eq!(rows(&conn), 2);

        // Deleting the chat takes its mapping...
        conn.execute("DELETE FROM chat_sessions WHERE id = 'chat-1'", [])
            .expect("delete chat");
        assert_eq!(rows(&conn), 1);
        // ...and so does deleting the integration.
        conn.execute("DELETE FROM integrations WHERE id = 'int-2'", [])
            .expect("delete integration");
        assert_eq!(rows(&conn), 0);
    }

    /// The replay guard: `slack_processed_events` keys on the pair, so Slack
    /// redelivering an event it believes failed cannot start a second run, while
    /// the same event id under another integration still can.
    #[test]
    fn a_slack_event_is_recorded_once_per_integration() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        apply(&mut conn).expect("apply");

        let record = |integration: &str, event: &str| {
            conn.execute(
                "INSERT INTO slack_processed_events (integration_id, event_id, processed_at)
                 VALUES (?1, ?2, '')",
                rusqlite::params![integration, event],
            )
        };
        record("int-1", "Ev1").expect("first delivery");
        record("int-1", "Ev1").expect_err("a redelivery is refused");
        record("int-2", "Ev1").expect("another integration is a different event");
    }

    /// Migration 39's five columns land on the existing rules, with the defaults
    /// that mean "what the dispatcher already does" — so the upgrade changes no
    /// behaviour for a rule nobody has edited since.
    #[test]
    fn the_execution_settings_default_to_the_dispatchers_own_behaviour() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        apply(&mut conn).expect("apply");

        conn.execute_batch(
            "INSERT INTO integrations (id, name, type, enabled, created_at, updated_at)
                  VALUES ('int-1', 'Telegram', 'telegram', 1, '', '');
             INSERT INTO trigger_rules (id, integration_id, agent_slug)
                  VALUES ('rule-1', 'int-1', 'a');",
        )
        .expect("a rule written the way an older build writes one");

        let (model, dir, profile, mode, timeout): (String, String, String, String, i64) = conn
            .query_row(
                "SELECT model, working_directory, settings_profile_id, permission_mode,
                        timeout_minutes
                   FROM trigger_rules WHERE id = 'rule-1'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("the new columns");
        assert_eq!((model.as_str(), dir.as_str()), ("", ""));
        assert_eq!((profile.as_str(), mode.as_str()), ("", ""));
        assert_eq!(timeout, 0);
    }

    /// **Migration 40's two columns are nullable, and absent is `NULL`** (#594).
    ///
    /// Unlike 30, 37, 38 and 39 they are not `NOT NULL DEFAULT`: a pid of 0 is
    /// a real signal target — `kill(0, …)` signals the caller's own process
    /// group — so "no process was recorded" must not share a spelling with any
    /// pid. Every row written before this migration, and every run that failed
    /// before it spawned, reads back as `NULL`.
    #[test]
    fn a_job_row_with_no_recorded_process_reads_back_null() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        apply(&mut conn).expect("apply");
        conn.execute_batch(
            "INSERT INTO scheduled_tasks (id, name, prompt) VALUES ('t1', 'T', 'p');
             INSERT INTO job_history (id, task_id, task_name, started_at)
             VALUES ('j1', 't1', 'T', '2026-01-01 00:00:00 +0000 UTC');",
        )
        .expect("seed");
        let (pid, started): (Option<i64>, Option<String>) = conn
            .query_row(
                "SELECT pid, pid_started_at FROM job_history WHERE id = 'j1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        assert_eq!((pid, started), (None, None));
    }

    /// **Migration 46 marks nothing expired and names Claude Code as the
    /// harness of every row already cached** (#704).
    ///
    /// Seeded at 45 rather than built fresh, because the rows that matter are
    /// the ones an existing install already has: a NULL `transcript_expired_at`
    /// is what says "the transcript is still there", and the default is what
    /// fills `harness` on rows written before the column existed.
    #[test]
    fn an_expired_marker_and_harness_default_on_existing_cache_rows() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        seed_at(&conn, 45);
        conn.execute_batch(
            "INSERT INTO claude_session_cache (session_id, project_path, file_path, file_mtime,
                 start_time, last_activity)
             VALUES ('s1', '/a', '/a/s1.jsonl', 'now', 'now', 'now'),
                    ('s2', '/b', '/b/s2.jsonl', 'now', 'now', 'now');
             INSERT INTO claude_subagent_cache (parent_session_id, agent_id, file_path, file_mtime)
             VALUES ('s1', 'a1', '/a/s1/subagents/agent-a1.jsonl', 'now');",
        )
        .expect("seed rows at 45");

        apply(&mut conn).expect("apply 46 and later");
        assert_eq!(current_version(&conn).expect("version"), 55);

        for table in ["claude_session_cache", "claude_subagent_cache"] {
            let (rows, untouched): (i64, i64) = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*), SUM(transcript_expired_at IS NULL AND harness = 'claude')
                         FROM {table}"
                    ),
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .expect("read back");
            assert!(rows > 0, "{table} was seeded");
            assert_eq!(
                rows, untouched,
                "{table}: every existing row keeps a transcript and is claude's"
            );
        }
    }

    /// **Migration 48 records today's behaviour rather than changing it**
    /// (#681).
    ///
    /// Seeded at 47, because the backfill is about the rows an install already
    /// has. Every existing rule continues on a reply and so does every task
    /// with a Slack destination, whose thread #642 maps unconditionally; a
    /// task that delivers nowhere, or only to Telegram, does not. A run
    /// recorded before the upgrade was started by the schedule as far as
    /// anything stored can tell.
    ///
    /// The last four tasks are the hand-edited `destinations` the `UPDATE`
    /// must survive: `json_each` raises on text that is not JSON and
    /// `json_extract` raises on a string element, and either would fail the
    /// whole upgrade and leave the app unable to start.
    #[test]
    fn existing_rules_and_slack_tasks_continue_on_reply_after_the_upgrade() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        seed_at(&conn, 47);
        conn.execute_batch(
            r#"INSERT INTO integrations (id, name, type, enabled, created_at, updated_at)
                    VALUES ('int-1', 'Telegram', 'telegram', 1, '', '');
               INSERT INTO trigger_rules (id, integration_id, agent_slug, enabled)
                    VALUES ('rule-on', 'int-1', 'a', 1), ('rule-off', 'int-1', 'a', 0);
               INSERT INTO scheduled_tasks (id, name, prompt, destinations) VALUES
                    ('slack', 'T', 'p', '[{"type":"slack","when":"always","slack":{"integration_id":"i","channels":["C0123ABCD"]}}]'),
                    ('mixed', 'T', 'p', '[{"type":"telegram"},{"type":"slack"}]'),
                    ('telegram', 'T', 'p', '[{"type":"telegram","when":"always"}]'),
                    ('plain', 'T', 'p', '[]'),
                    ('not-json', 'T', 'p', 'slack, by hand'),
                    ('strings', 'T', 'p', '["slack", 7, null]'),
                    ('object', 'T', 'p', '{"type":"slack"}'),
                    ('blank', 'T', 'p', '');
               INSERT INTO job_history (id, task_id, task_name, started_at)
                    VALUES ('j1', 'plain', 'T', '2026-01-01 00:00:00 +0000 UTC');"#,
        )
        .expect("seed rows at 47");

        apply(&mut conn).expect("apply 48 and later");
        assert_eq!(current_version(&conn).expect("version"), 55);

        let flagged = |table: &str| -> Vec<String> {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT id FROM {table} WHERE continue_on_reply = 1 ORDER BY id"
                ))
                .expect("prepare");
            let ids = stmt
                .query_map([], |row| row.get(0))
                .expect("query")
                .collect::<Result<Vec<String>, _>>()
                .expect("rows");
            ids
        };
        assert_eq!(flagged("trigger_rules"), ["rule-off", "rule-on"]);
        assert_eq!(flagged("scheduled_tasks"), ["mixed", "slack"]);

        let (rule_task, dropped, limited): (String, i64, i64) = conn
            .query_row(
                "SELECT (SELECT task_id FROM trigger_rules WHERE id = 'rule-on'),
                        dropped_event_count, rate_limited_event_count
                   FROM scheduled_tasks WHERE id = 'slack'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("the link and the counters");
        assert_eq!((rule_task.as_str(), dropped, limited), ("", 0, 0));

        let (by, continues, payload): (String, String, String) = conn
            .query_row(
                "SELECT triggered_by, continues_job_id, event_payload
                   FROM job_history WHERE id = 'j1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("the run's new columns");
        assert_eq!(
            (by.as_str(), continues.as_str(), payload.as_str()),
            ("schedule", "", "")
        );

        // What is written after the upgrade defaults to off: the backfill is
        // for what existed, not a new default.
        conn.execute_batch(
            r#"INSERT INTO trigger_rules (id, integration_id, agent_slug)
                    VALUES ('rule-new', 'int-1', 'a');
               INSERT INTO scheduled_tasks (id, name, prompt, destinations)
                    VALUES ('slack-new', 'T', 'p', '[{"type":"slack"}]');"#,
        )
        .expect("rows written after the upgrade");
        assert_eq!(flagged("trigger_rules"), ["rule-off", "rule-on"]);
        assert_eq!(flagged("scheduled_tasks"), ["mixed", "slack"]);
    }

    /// **The install's `machine_id` is made once, by the migration, and kept**
    /// (#704).
    ///
    /// One row of 32 lowercase hex characters; a second `apply` and a reopen of
    /// the file leave it as it was, because nothing but migration 46 writes it
    /// and that runs once per database. Two databases get two ids — the value
    /// itself is random, so this asserts shape and stability, never a value.
    #[test]
    fn the_machine_id_is_generated_once_and_survives_reapply() {
        fn machine_ids(conn: &Connection) -> Vec<String> {
            let mut stmt = conn
                .prepare("SELECT machine_id FROM install_identity")
                .expect("prepare");
            stmt.query_map([], |row| row.get(0))
                .expect("query")
                .collect::<Result<_, _>>()
                .expect("rows")
        }

        let file = tempfile::NamedTempFile::new().expect("temp file");
        let first = {
            let mut conn = Connection::open(file.path()).expect("open");
            apply(&mut conn).expect("apply");
            let ids = machine_ids(&conn);
            assert_eq!(ids.len(), 1, "exactly one install identity");
            let id = ids[0].clone();
            assert_eq!(id.len(), 32, "{id}");
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
                "{id} is lowercase hex"
            );

            apply(&mut conn).expect("second apply");
            assert_eq!(machine_ids(&conn), vec![id.clone()], "a re-apply keeps it");
            id
        };

        let mut reopened = Connection::open(file.path()).expect("reopen");
        apply(&mut reopened).expect("apply after reopen");
        assert_eq!(
            machine_ids(&reopened),
            vec![first.clone()],
            "a restart keeps it"
        );

        let other = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(other.path()).expect("open");
        apply(&mut conn).expect("apply");
        assert_ne!(
            machine_ids(&conn),
            vec![first],
            "each database gets its own id"
        );

        // The single-row rule is the table's, not merely this test's.
        conn.execute(
            "INSERT INTO install_identity (id, machine_id) VALUES (2, 'x')",
            [],
        )
        .expect_err("install_identity holds one row");
    }

    /// **Migration 41's tables key on the session pair, and a rescan of the
    /// same match is refused rather than duplicated** (#600).
    ///
    /// `credential_scan_state` is what records a clean scan, so it must hold
    /// one session under two project paths as two rows; `credential_findings`'
    /// UNIQUE key is what makes re-scanning an unchanged transcript a no-op.
    #[test]
    fn the_credential_tables_key_on_the_session_pair() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        apply(&mut conn).expect("apply");
        conn.execute_batch(
            "INSERT INTO credential_scan_state (session_id, project_path, ruleset_version, scanned_at)
             VALUES ('s1', '/a', 1, 'now'), ('s1', '/b', 1, 'now');
             INSERT INTO credential_findings (session_id, project_path, rule_id, confidence,
                 masked_snippet, location_start, location_end, ruleset_version, detected_at)
             VALUES ('s1', '/a', 'aws-access-key', 'high', 'AKIA****', 10, 30, 1, 'now');
             INSERT INTO credential_whitelist (rule_id, created_at) VALUES ('aws-access-key', 'now');",
        )
        .expect("seed");

        let status: String = conn
            .query_row("SELECT status FROM credential_findings", [], |row| {
                row.get(0)
            })
            .expect("status");
        assert_eq!(status, "open");

        // Asserted on the message, not merely on `is_err`: a typo in this
        // statement would also fail, and would pass without the key existing.
        let duplicate = conn
            .execute(
                "INSERT INTO credential_findings (session_id, project_path, rule_id, confidence,
                     masked_snippet, location_start, location_end, ruleset_version, detected_at)
                 VALUES ('s1', '/a', 'aws-access-key', 'high', 'AKIA****', 10, 30, 1, 'later')",
                [],
            )
            .expect_err("the same match must not be stored twice");
        assert!(
            duplicate
                .to_string()
                .contains("UNIQUE constraint failed: credential_findings."),
            "{duplicate}"
        );
    }

    /// Idempotence, which is what makes a second process safe to run at all.
    #[test]
    fn applying_twice_is_a_no_op() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");

        apply(&mut conn).expect("first");
        apply(&mut conn).expect("second must not fail");
        assert_eq!(current_version(&conn).expect("version"), 55);
    }

    /// **The upgrade path a real install takes**, which neither the
    /// fresh-database test nor sequential idempotence covers.
    ///
    /// Both of those start from nothing and run the whole list. A user's
    /// database is at whatever version their last build shipped, so the case
    /// that matters is "apply only the tail" — and it is the case where an
    /// appended migration that is not additive fails: a `CREATE TABLE` without
    /// `IF NOT EXISTS` against a table an earlier migration already made, or an
    /// `ALTER` of a column that has since been dropped, passes the fresh test
    /// and breaks on every existing install.
    ///
    /// Written as a loop over every intermediate version rather than against a
    /// single one, so it keeps covering the newest migration without being
    /// edited — the count above is already the thing that has to be bumped by
    /// hand, and one such place is enough.
    #[test]
    fn a_database_at_any_earlier_version_upgrades_to_this_one() {
        let all = migrations();

        for stop_at in 1..all.len() {
            let file = tempfile::NamedTempFile::new().expect("temp file");
            let mut conn = Connection::open(file.path()).expect("open");
            seed_at(&conn, stop_at);

            apply(&mut conn)
                .unwrap_or_else(|e| panic!("upgrading from version {stop_at} failed: {e}"));
            assert_eq!(
                current_version(&conn).expect("version"),
                expected_version(),
                "a database at {stop_at} did not reach this build's version"
            );
            verify(&conn)
                .unwrap_or_else(|e| panic!("upgrade from {stop_at} left it unusable: {e}"));
        }
    }

    /// Seed a database at `stop_at` by applying only that prefix, through the
    /// same statements `apply` uses.
    fn seed_at(conn: &Connection, stop_at: usize) {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                 version    INTEGER PRIMARY KEY,
                 applied_at DATETIME NOT NULL
             );",
        )
        .expect("migration table");
        for migration in &migrations()[..stop_at] {
            conn.execute_batch(&migration.sql)
                .unwrap_or_else(|e| panic!("seeding migration {}: {e}", migration.version));
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                rusqlite::params![migration.version, crate::native::gotime::now_go_text()],
            )
            .expect("record");
        }
        assert_eq!(
            current_version(conn).expect("version"),
            stop_at as i64,
            "seeding did not land on the expected version"
        );
    }

    /// Migration 50 (#676): a Telegram row whose webhook was active comes out
    /// with the inbound switch on, so its triggers move to the long poll with
    /// no user action. Nothing else is touched — a Telegram row with no active
    /// webhook never received anything, and a Slack row's switch is its own.
    #[test]
    fn migration_50_turns_inbound_on_for_an_active_telegram_webhook() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        seed_at(&conn, 49);
        for (id, kind, status) in [
            ("tg-active", "telegram", "active"),
            ("tg-inactive", "telegram", "inactive"),
            ("tg-never", "telegram", ""),
            ("tg-error", "telegram", "error"),
            ("sl", "slack", "active"),
        ] {
            conn.execute(
                "INSERT INTO integrations
                    (id, name, type, enabled, credentials, services, webhook_status,
                     created_at, updated_at)
                 VALUES (?1, ?1, ?2, 1, '{}', '{}', ?3, 'then', 'then')",
                rusqlite::params![id, kind, status],
            )
            .expect("seed");
        }

        apply(&mut conn).expect("apply 50");

        let switched: Vec<String> = conn
            .prepare("SELECT id FROM integrations WHERE inbound_enabled = 1 ORDER BY id")
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        assert_eq!(switched, ["tg-active"]);
        let updated_at: String = conn
            .query_row(
                "SELECT updated_at FROM integrations WHERE id = 'tg-active'",
                [],
                |row| row.get(0),
            )
            .expect("read");
        assert_eq!(updated_at, "then", "a backfill is not a user edit");
    }

    /// Migration 55 (#741): a database URL snippet stored under ruleset 1 on a
    /// short host loses the `@` and the password characters before it, without
    /// a rescan — the session here has expired, so no rescan will ever come.
    /// Every other row is left as it was, and a second run changes nothing.
    #[test]
    fn migration_55_rewrites_a_database_url_snippet_that_kept_the_password_tail() {
        let mut conn = Connection::open_in_memory().expect("in-memory db");
        seed_at(&conn, 54);
        conn.execute_batch(
            "INSERT INTO claude_session_cache
                 (session_id, project_path, file_path, file_mtime, start_time, last_activity,
                  transcript_expired_at)
             VALUES ('gone', '/a', '/a/gone.jsonl', '2026-01-01 00:00:00+00:00',
                     '2026-01-01 00:00:00+00:00', '2026-01-01 00:00:00+00:00',
                     '2026-02-01 00:00:00+00:00');
             INSERT INTO credential_findings
                 (session_id, project_path, rule_id, location_start, confidence,
                  masked_snippet, location_end, ruleset_version, match_hash, detected_at)
             VALUES
               ('gone', '/a', 'database-url-credentials', 10, 'high', 'post********K@db', 40, 1, 'h1', 't'),
               ('gone', '/a', 'database-url-credentials', 50, 'high', 'mysq********ZK@h', 80, 1, 'h2', 't'),
               ('gone', '/a', 'database-url-credentials', 90, 'high', 'post********@pg1', 120, 1, 'h3', 't'),
               ('gone', '/a', 'database-url-credentials', 130, 'high', 'post********.com', 170, 1, 'h4', 't'),
               ('gone', '/a', 'database-url-credentials', 180, 'high', '********', 190, 1, 'h5', 't'),
               ('gone', '/a', 'github-pat', 200, 'high', 'ghp_********a@bc', 240, 1, 'h6', 't');",
        )
        .expect("seed rows at 54");

        let snippets = |conn: &Connection| -> Vec<String> {
            let mut stmt = conn
                .prepare("SELECT masked_snippet FROM credential_findings ORDER BY location_start")
                .expect("prepare");
            stmt.query_map([], |r| r.get(0))
                .expect("query")
                .collect::<Result<Vec<_>, _>>()
                .expect("rows")
        };
        let expected = [
            "post********db",
            "mysq********h",
            "post********pg1",
            "post********.com",
            "********",
            // Another rule's snippet is not this migration's to touch.
            "ghp_********a@bc",
        ];

        apply(&mut conn).expect("apply 55");
        assert_eq!(snippets(&conn), expected);

        let sql = &migrations()[54].sql;
        conn.execute_batch(sql).expect("a second run");
        assert_eq!(snippets(&conn), expected);
    }

    /// Migration 54 (#691): a task that existed before it reads back with the
    /// default limits, and an older build's insert, which names none of the
    /// three columns, still lands with them.
    #[test]
    fn migration_54_gives_every_task_the_default_event_run_limits() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        seed_at(&conn, 53);
        let insert = |conn: &Connection, id: &str| {
            conn.execute(
                "INSERT INTO scheduled_tasks (id, name, prompt) VALUES (?1, 'T', 'p')",
                [id],
            )
            .expect("insert task");
        };
        insert(&conn, "existing");

        apply(&mut conn).expect("apply 54");
        insert(&conn, "older-build");

        let rows: Vec<(String, i64, i64, i64)> = conn
            .prepare(
                "SELECT id, max_concurrent_runs, max_queued_events, max_runs_per_hour
                 FROM scheduled_tasks ORDER BY id",
            )
            .expect("prepare")
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        assert_eq!(
            rows,
            vec![
                ("existing".to_string(), 1, 5, 10),
                ("older-build".to_string(), 1, 5, 10),
            ]
        );
    }

    /// Migration 53 (#688): a rule that existed before it reads back with an
    /// empty `filter_user_ids` and the `enabled` it had, and an older build's
    /// insert, which does not name the column, still lands.
    #[test]
    fn migration_53_gives_existing_rules_an_empty_user_list_and_keeps_them_on() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        seed_at(&conn, 52);
        conn.execute(
            "INSERT INTO integrations
                (id, name, type, enabled, credentials, services, created_at, updated_at)
             VALUES ('sl', 'sl', 'slack', 1, '{}', '{}', 'then', 'then')",
            [],
        )
        .expect("seed integration");
        let insert = |conn: &Connection, id: &str, enabled: i64| {
            conn.execute(
                "INSERT INTO trigger_rules
                    (id, integration_id, name, agent_slug, enabled, created_at, updated_at)
                 VALUES (?1, 'sl', ?1, 'a', ?2, 'then', 'then')",
                rusqlite::params![id, enabled],
            )
            .expect("insert rule");
        };
        insert(&conn, "on", 1);
        insert(&conn, "off", 0);

        apply(&mut conn).expect("apply 53");
        insert(&conn, "older-build", 1);

        let rows: Vec<(String, i64, String)> = conn
            .prepare("SELECT id, enabled, filter_user_ids FROM trigger_rules ORDER BY id")
            .expect("prepare")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        let empty = || "[]".to_string();
        assert_eq!(
            rows,
            vec![
                ("off".to_string(), 0, empty()),
                ("older-build".to_string(), 1, empty()),
                ("on".to_string(), 1, empty()),
            ]
        );
    }

    /// Migration 52 (#678): every run recorded before it comes out naming this
    /// install and `claude`, because nothing else could have run it. Seeded at
    /// 51, since the backfill is about the rows an install already has. The
    /// last insert is an older build's — it names neither column — and must
    /// still land, which is what "additive" promises.
    #[test]
    fn migration_52_backfills_existing_runs_with_this_install_and_claude() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        seed_at(&conn, 51);
        conn.execute_batch(
            "INSERT INTO scheduled_tasks (id, name, prompt) VALUES ('t1', 'T', 'p');
             INSERT INTO job_history (id, task_id, task_name, started_at, triggered_by)
             VALUES ('old-1', 't1', 'T', '2026-01-01 00:00:00 +0000 UTC', 'schedule'),
                    ('old-2', 't1', 'T', '2026-01-02 00:00:00 +0000 UTC', 'slack');",
        )
        .expect("seed runs at 51");

        apply(&mut conn).expect("apply 52");

        let machine_id: String = conn
            .query_row("SELECT machine_id FROM install_identity", [], |row| {
                row.get(0)
            })
            .expect("identity");
        assert_eq!(machine_id.len(), 32, "{machine_id:?}");
        let rows = |conn: &Connection| -> Vec<(String, String, String)> {
            conn.prepare("SELECT id, machine_id, harness FROM job_history ORDER BY id")
                .expect("prepare")
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .expect("query")
                .collect::<Result<_, _>>()
                .expect("rows")
        };
        let claude = || "claude".to_string();
        assert_eq!(
            rows(&conn),
            vec![
                ("old-1".to_string(), machine_id.clone(), claude()),
                ("old-2".to_string(), machine_id.clone(), claude()),
            ]
        );

        conn.execute(
            "INSERT INTO job_history (id, task_id, task_name, started_at)
             VALUES ('older-build', 't1', 'T', '2026-01-03 00:00:00 +0000 UTC')",
            [],
        )
        .expect("an insert that names neither column still lands");
        assert_eq!(
            rows(&conn)[2],
            ("older-build".to_string(), String::new(), claude())
        );
    }

    /// Migration 51 (#674): an enabled Telegram rule whose `filter_chat_ids`
    /// names no chat comes out turned off, whatever shape "names no chat"
    /// takes in the column. A rule that lists one, a rule already off and a
    /// Slack rule (where an empty list is the workspace default) are untouched.
    #[test]
    fn migration_51_turns_off_telegram_rules_that_list_no_chat() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = Connection::open(file.path()).expect("open");
        seed_at(&conn, 50);
        for (id, kind) in [("tg", "telegram"), ("sl", "slack")] {
            conn.execute(
                "INSERT INTO integrations
                    (id, name, type, enabled, credentials, services, created_at, updated_at)
                 VALUES (?1, ?1, ?2, 1, '{}', '{}', 'then', 'then')",
                [id, kind],
            )
            .expect("seed integration");
        }
        for (id, integration, enabled, chat_ids) in [
            ("tg-empty", "tg", 1, "[]"),
            ("tg-null", "tg", 1, "null"),
            ("tg-blank", "tg", 1, ""),
            ("tg-broken", "tg", 1, "not json"),
            ("tg-object", "tg", 1, r#"{"42":1}"#),
            ("tg-string", "tg", 1, r#""42""#),
            ("tg-blank-entry", "tg", 1, r#"["",null]"#),
            ("tg-number-entry", "tg", 1, "[42]"),
            ("tg-listed", "tg", 1, r#"["42"]"#),
            ("tg-listed-among-blanks", "tg", 1, r#"["","-100"]"#),
            ("tg-off-empty", "tg", 0, "[]"),
            ("sl-empty", "sl", 1, "[]"),
            ("sl-listed", "sl", 1, r#"["C1"]"#),
        ] {
            conn.execute(
                "INSERT INTO trigger_rules
                    (id, integration_id, name, agent_slug, enabled, filter_chat_ids,
                     model, created_at, updated_at)
                 VALUES (?1, ?2, ?1, 'a', ?3, ?4, 'opus', 'then', 'then')",
                rusqlite::params![id, integration, enabled, chat_ids],
            )
            .expect("seed rule");
        }

        apply(&mut conn).expect("apply 51");

        let on: Vec<String> = conn
            .prepare("SELECT id FROM trigger_rules WHERE enabled = 1 ORDER BY id")
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        assert_eq!(
            on,
            [
                "sl-empty",
                "sl-listed",
                "tg-listed",
                "tg-listed-among-blanks"
            ]
        );
        // Turned off, not rewritten: the rule keeps everything else it held.
        let kept: (i64, String, String, String) = conn
            .query_row(
                "SELECT COUNT(*), MIN(model), MAX(model), MAX(updated_at) FROM trigger_rules",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("read");
        assert_eq!(kept, (13, "opus".into(), "opus".into(), "then".into()));
        let chat_ids: String = conn
            .query_row(
                "SELECT filter_chat_ids FROM trigger_rules WHERE id = 'tg-broken'",
                [],
                |row| row.get(0),
            )
            .expect("read");
        assert_eq!(chat_ids, "not json");
    }

    /// The property this whole function exists for, and the one sequential
    /// idempotence does **not** prove: two processes applying at once must both
    /// succeed rather than one failing on duplicate DDL — which is exactly what
    /// Go's runner does, because it reads the version outside the transaction
    /// that applies the next migration.
    #[test]
    fn two_connections_applying_concurrently_both_succeed() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();

        // WAL is set **once, up front** — not per thread.
        //
        // It is persistent in the file, so both connections inherit it. Setting
        // it inside each thread is what made this flake in CI: switching journal
        // mode needs a lock SQLite refuses to *wait* for, so it returns
        // `SQLITE_BUSY` immediately rather than honouring a busy timeout, and
        // the test failed on its own setup instead of on the property it
        // measures. The app never hits this because the mode is already set by
        // the time anything opens the database.
        {
            let conn = Connection::open(&path).expect("open");
            conn.pragma_update(None, "journal_mode", "WAL")
                .expect("wal");
        }

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let path = path.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let mut conn = Connection::open(&path).expect("open");
                    barrier.wait();
                    apply(&mut conn)
                })
            })
            .collect();

        for handle in handles {
            handle
                .join()
                .expect("thread")
                .expect("both must apply cleanly");
        }

        let conn = Connection::open(&path).expect("open");
        assert_eq!(current_version(&conn).expect("version"), 55);
        // Each migration recorded exactly once — a double-apply would have
        // violated the primary key and failed above, but assert the end state
        // rather than relying on that.
        let recorded: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("count");
        assert_eq!(recorded, 55);
    }

    #[test]
    fn a_database_behind_this_build_is_refused() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = Connection::open(file.path()).expect("open");
        conn.execute_batch(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at DATETIME);
             INSERT INTO schema_migrations (version, applied_at) VALUES (26, '');",
        )
        .expect("seed");

        let err = verify(&conn).expect_err("a behind database must not be served");
        // The two directions must stay distinguishable in the message — a log
        // line that says which one saves the next person a bisect.
        assert!(err.contains("have not been applied"), "got: {err}");
        assert!(
            err.contains("26"),
            "the stored version must be named: {err}"
        );
    }

    /// A newer database is refused, not accepted silently: this build's queries
    /// were compiled against its own schema, not that one.
    #[test]
    fn a_database_ahead_of_this_build_is_refused() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = Connection::open(file.path()).expect("open");
        conn.execute_batch(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at DATETIME);
             INSERT INTO schema_migrations (version, applied_at) VALUES (99, '');",
        )
        .expect("seed");

        let err = verify(&conn).expect_err("a newer database must not be served");
        assert!(err.contains("newer than this build"), "got: {err}");
    }

    /// No `schema_migrations` at all is not "version 0" — it is not an Agento
    /// database, and guessing would mean writing into someone else's file.
    #[test]
    fn a_database_with_no_migration_table_is_an_error() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let conn = Connection::open(file.path()).expect("open");
        assert!(current_version(&conn).is_err());
        assert!(verify(&conn).is_err());
    }
}
