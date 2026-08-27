//! The journey builder against the machine's **real** corpus.
//!
//! `#[ignore]`d, like `scan_live.rs`, `insights_live.rs` and `search_live.rs`,
//! and for the same reason — CI has no `~/.claude` and no
//! `~/.agento/agento.db`. Run it by hand:
//!
//! ```bash
//! cargo test --test journey_live -- --ignored --nocapture
//! ```
//!
//! # Why this exists rather than another fixture
//!
//! `journey.rs` already carries a unit test per rule and a byte-exact golden,
//! and none of them can see the two failures that matter most here. (No count
//! here on purpose: a number in prose drifts the next time a test is added.)
//!
//! The first is a builder that runs, reports success and produces **no turns** —
//! the shape #408 had on the insight side. A three-file fixture with one prompt
//! in it cannot tell that from a healthy build, because a real transcript is
//! nothing like the fixture: it opens with a slash-command expansion, its
//! prompts arrive between hundreds of tool-result carriers, and most of its
//! events are types the switch ignores. So the assertion is a *ratio over the
//! real corpus*: the sessions that have genuine turns must overwhelmingly
//! produce them.
//!
//! The second is turn-predicate drift, which is only visible at scale for the
//! same reason. Every sampled session's turn count is compared against what the
//! insight pipeline computes over the same file, in the same run — so a change
//! that makes the journey disagree with `turn_count` on the Insights page fails
//! here rather than being noticed by a user comparing two pages.
//!
//! It also prints the timings, because `journey.rs`'s own risk note says to
//! measure a per-request whole-transcript re-read against a real corpus before
//! shipping one. Take the numbers under `--release`; the `bundled` SQLite and
//! the JSON decode are both compiled at the profile's optimization level.

use std::path::{Path, PathBuf};
use std::time::Instant;

use agento_lib::native::insights::{index::DocAccumulator, processors};
use agento_lib::native::sessions::{detail, journey};
use agento_lib::native::settings;

/// One cached session, and the two active figures the list would report for it.
struct Row {
    session_id: String,
    subagents: i64,
    cached_active_ms: i64,
    cached_subagent_active_ms: i64,
}

/// How many sessions to walk. The whole corpus would take minutes; this is
/// enough for the ratio and the drift check to mean something.
const SAMPLE: usize = 60;

fn real_db() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let db = PathBuf::from(home).join(".agento/agento.db");
    db.is_file().then_some(db)
}

/// Copy the database *and its WAL*, so the copy is not missing recent writes.
fn copy_db(src: &Path, dir: &Path) -> PathBuf {
    let db = dir.join("agento.db");
    std::fs::copy(src, &db).expect("copy the database");
    for ext in ["-wal", "-shm"] {
        let from = PathBuf::from(format!("{}{ext}", src.display()));
        if from.is_file() {
            let _ = std::fs::copy(&from, dir.join(format!("agento.db{ext}")));
        }
    }
    db
}

#[test]
#[ignore = "needs the machine's real ~/.agento database and ~/.claude corpus"]
fn the_journey_builds_over_the_real_corpus() {
    let Some(src) = real_db() else {
        eprintln!("skipping: no ~/.agento/agento.db");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let db = copy_db(&src, dir.path());

    let conn = rusqlite::Connection::open(&db).expect("open the copy");
    // Sessions with the most delegated work first: they exercise the nesting,
    // the unmatched-sub-agent path and the absorbed stamps at once.
    let mut stmt = conn
        .prepare(
            "SELECT c.session_id,
                    (SELECT COUNT(*) FROM claude_subagent_cache s
                      WHERE s.parent_session_id = c.session_id) AS subagents,
                    c.active_duration_ms,
                    (SELECT COALESCE(SUM(s.active_duration_ms), 0)
                       FROM claude_subagent_cache s
                      WHERE s.parent_session_id = c.session_id) AS sub_active
             FROM claude_session_cache c
             ORDER BY subagents DESC, c.message_count DESC
             LIMIT ?1",
        )
        .expect("prepare");
    let rows: Vec<Row> = stmt
        .query_map([SAMPLE as i64], |r| {
            Ok(Row {
                session_id: r.get(0)?,
                subagents: r.get(1)?,
                cached_active_ms: r.get(2)?,
                cached_subagent_active_ms: r.get(3)?,
            })
        })
        .expect("query")
        .filter_map(Result::ok)
        .collect();
    drop(stmt);
    assert!(
        !rows.is_empty(),
        "the copied database holds no cached sessions — scan first"
    );

    let dirs = settings::load(&conn).indexed_config_dirs;
    let idle_gap_ms = settings::load(&conn).idle_gap_ms;

    let mut built = 0usize;
    let mut with_turns = 0usize;
    let mut with_subagents = 0usize;
    let mut nested_or_appended = 0usize;
    let mut total_steps = 0usize;
    let mut orphan_headed = 0usize;
    let mut below_the_sum = 0usize;
    let mut slowest = (0u128, String::new());
    let mut elapsed_total = 0u128;

    for Row {
        session_id,
        subagents: cached_subagents,
        cached_active_ms,
        cached_subagent_active_ms,
    } in &rows
    {
        let start = Instant::now();
        let journey = journey::get(&db, session_id).expect("journey read");
        let took = start.elapsed().as_millis();
        elapsed_total += took;
        if took > slowest.0 {
            slowest = (took, session_id.clone());
        }

        let Some(journey) = journey else {
            // A cached row whose transcript has since been removed. Not a
            // failure — the endpoint's own 404 — but worth seeing.
            eprintln!("  {session_id}: no transcript on disk");
            continue;
        };
        built += 1;
        total_steps += journey.turns.iter().map(|t| t.steps.len()).sum::<usize>();
        if !journey.turns.is_empty() {
            with_turns += 1;
        }
        if journey.subagent_count > 0 {
            with_subagents += 1;
            let delegated = journey.turns.iter().any(|t| {
                t.steps
                    .iter()
                    .any(|s| s.step_type == "sub_agent" || !s.steps.is_empty())
            });
            assert!(
                delegated,
                "{session_id} reports {} sub-agents and renders none of them",
                journey.subagent_count
            );
            nested_or_appended += 1;
        }
        assert_eq!(
            journey.subagent_count, *cached_subagents,
            "{session_id}: the journey counted {} sub-agents where the cache row \
             has {cached_subagents} — the two walk the same directory",
            journey.subagent_count
        );
        assert_eq!(
            journey.total_turns as usize,
            journey.turns.len(),
            "{session_id}: total_turns disagrees with the turns it shipped"
        );

        // The one bound that is actually an invariant, and it is one-directional
        // by construction: `durations()` is `Σ min(gap, cap)` over consecutive
        // stamps, so absorbing more stamps can only subdivide gaps and never
        // lower the total. That makes this sound because the journey's stamp set
        // is a strict **superset** of the scanner's — `Builder::process_event`
        // observes every timestamped event, while `read_summary_file` observes
        // only those `bounds_session_time_range` admits (eight further denied
        // types beyond `file-history-snapshot`). So a future change that
        // *narrows* what the journey observes is the direction that breaks this,
        // and it is the direction the assertion catches.
        //
        // There is deliberately **no upper bound against the sessions list's
        // sum**, and the reason is the trap this suite exists to avoid stating
        // as fact: subdividing a gap that was already longer than the cap
        // replaces one capped gap with two, so the merged figure can legitimately
        // come out *larger* than the sum — one sidecar with a single logged event
        // inside a long parent gap is enough. It happens not to on this corpus,
        // which is exactly why asserting it would be a test that flakes on
        // somebody else's. So the relationship is counted and reported instead.
        assert!(
            journey.active_duration_ms >= *cached_active_ms,
            "{session_id}: the journey's active time ({}) is below the parent's \
             own cached figure ({cached_active_ms}) — absorbing stamps cannot \
             lower the total, so a stamp was lost",
            journey.active_duration_ms
        );
        if journey.active_duration_ms < cached_active_ms + cached_subagent_active_ms {
            below_the_sum += 1;
        }

        // The anti-drift check, over the same file in the same run: both sides
        // must resolve "is this a turn?" through the one predicate.
        let Some((_, _, file)) = detail::find_session_file(&dirs, session_id) else {
            continue;
        };
        let insight = processors::run(
            session_id,
            &[file],
            &processors::Ctx {
                idle_gap_ms,
                resolver: None,
            },
            &mut DocAccumulator::new(),
        )
        .expect("insight");

        // There is **one** permitted difference, and it is structural rather
        // than a disagreement about the predicate: a transcript whose events
        // before the first genuine prompt include an assistant, a system or a
        // tool-result carrier gets a leading turn from `ensure_turn`, because
        // those steps must land somewhere — while the pipeline counts no turn
        // for them. It is not exotic; every session a slash command opened is
        // this shape, since the expansion and the skill preamble are both
        // injected wrappers and the model answers before the person types.
        //
        // So the check is exact in both directions rather than relaxed to a
        // tolerance: equal, or one more *and* that extra turn is a leading turn
        // with no `user_input` step in it. A genuine predicate drift fails one
        // of the two halves.
        let extra = journey.total_turns - insight.turn_count;
        let leading_orphan = journey
            .turns
            .first()
            .is_some_and(|t| t.steps.iter().all(|s| s.step_type != "user_input"));
        assert!(
            extra == 0 || (extra == 1 && leading_orphan),
            "{session_id}: the journey rendered {} turns and the insight pipeline \
             counted {} over the same transcript (leading orphan turn: \
             {leading_orphan}) — the shared predicate has drifted",
            journey.total_turns,
            insight.turn_count
        );
        if extra == 1 {
            orphan_headed += 1;
        }
    }

    eprintln!("\n── journey over the real corpus ──");
    eprintln!("  sessions sampled       {}", rows.len());
    eprintln!("  journeys built         {built}");
    eprintln!("  with at least one turn {with_turns}");
    eprintln!("  with sub-agents        {with_subagents} (all rendered: {nested_or_appended})");
    eprintln!("  steps built            {total_steps}");
    eprintln!("  led by orphan steps    {orphan_headed} (one turn more than turn_count)");
    // Over `built`, not `rows.len()`: a row whose transcript is gone never
    // reaches the comparison, so the sampled total would understate the share.
    eprintln!(
        "  active below list sum  {below_the_sum}/{built} (concurrent delegation counted once)"
    );
    eprintln!(
        "  mean per journey       {} ms",
        elapsed_total / (rows.len() as u128).max(1)
    );
    eprintln!("  slowest                {} ms  ({})", slowest.0, slowest.1);

    // The failure this suite exists for: a builder that answers and produces
    // nothing. A real corpus's sessions overwhelmingly have genuine turns —
    // the exceptions are the fully autonomous runs whose every user event is a
    // wrapper, which legitimately have none.
    assert!(built > 0, "no journey was built at all");
    assert!(
        with_turns * 4 >= built * 3,
        "only {with_turns} of {built} journeys produced a turn — the builder is \
         answering and rendering nothing"
    );
    assert!(total_steps > built, "a journey averaged less than one step");
}
