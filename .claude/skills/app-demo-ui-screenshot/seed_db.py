#!/usr/bin/env python3
"""Seed chats and job history straight into the scratch DB (run with the app stopped)."""
import json, os, sqlite3, sys, uuid, datetime as dt, random

random.seed(7)
DB = sys.argv[1]
NOW = dt.datetime(2026, 8, 23, 10, 40, tzinfo=dt.timezone.utc)
TOOL_HOME = "/home/dev"


def gt(t):
    return t.strftime("%Y-%m-%d %H:%M:%S.") + f"{t.microsecond * 1000:09d}" + " +0000 UTC"


conn = sqlite3.connect(DB)
conn.execute("PRAGMA foreign_keys=ON")
conn.execute("DELETE FROM chat_messages")
conn.execute("DELETE FROM chat_sessions")
conn.execute("DELETE FROM job_history")

CHATS = [
    # title, agent, cwd, model, minutes_ago, [(role, content, blocks)]
    ("Review PR #518 before merge", "code-reviewer", os.path.expanduser("~") + "/Projects/gateway", "claude-opus-5", 38, [
        ("user", "Review acme/gateway#518 (the upload validation fix). Correctness and security only.", None),
        ("assistant", "", [
            {"type": "text", "text": "Pulling the diff first, then I'll read the two files it touches in full."},
            {"type": "tool_use", "id": "toolu_01PR518diffAAAAAAAAAAAA", "name": "mcp__6b51c330-5c60-4d87-9a7c-d341455f2810__get_pull_diff",
             "input": {"owner": "acme", "repo": "gateway", "pull_number": 518}}]),
        ("assistant", "", [
            {"type": "tool_use", "id": "toolu_01PR518readBBBBBBBBBBBB", "name": "Read",
             "input": {"file_path": f"{TOOL_HOME}/Projects/gateway/internal/http/upload_sanitize.go"}}]),
        ("assistant",
         "**Verdict: safe to merge, one follow-up.**\n\n"
         "- `sanitizeExtension` is an allowlist and runs `filepath.Ext(filepath.Base(...))`, so a name containing a separator has no extension and is rejected. Good.\n"
         "- `sniffContentType` seeks back to the start before the save, which is what the failing test was about. Good.\n"
         "- The generated file name is a UUID, so nothing client-supplied reaches the path.\n\n"
         "Follow-up (not blocking): `avatar.go:58` still passes a hardcoded `image/png` that is now ignored by the sniffer. Drop the parameter so the next reader does not think it matters.",
         None),
    ]),
    ("Why is the reindex job slow?", "repo-librarian", os.path.expanduser("~") + "/Projects/search", "claude-haiku-4-5-20251001", 190, [
        ("user", "Where does the time go in the full reindex job? Just point me at the code, don't change anything.", None),
        ("assistant", "", [
            {"type": "tool_use", "id": "toolu_01REIDXgrepCCCCCCCCCCCC", "name": "Grep",
             "input": {"pattern": "func (.*) Reindex", "path": f"{TOOL_HOME}/Projects/search", "output_mode": "content"}}]),
        ("assistant", "", [
            {"type": "tool_use", "id": "toolu_01REIDXreadDDDDDDDDDDDD", "name": "Read",
             "input": {"file_path": f"{TOOL_HOME}/Projects/search/internal/index/reindex.go"}}]),
        ("assistant",
         "Three places, in order of cost:\n\n"
         "1. `internal/index/reindex.go:71` — documents are fetched one row at a time inside the loop (`store.Get(id)`), so it is N round-trips. Batch it.\n"
         "2. `internal/index/reindex.go:88` — `index.Flush()` is called per document rather than per batch.\n"
         "3. `internal/index/tokenizer.go:40` — the tokenizer is rebuilt for every document; it is safe to construct once.\n\n"
         "The first two account for most of the 40 minutes; the tokenizer is a few percent.",
         None),
    ]),
    ("Draft the August release notes", "release-notes", os.path.expanduser("~") + "/Projects/platform", "claude-sonnet-5", 1500, [
        ("user", "Draft the release notes for everything merged since v2.3. Don't write the file yet, show me first.", None),
        ("assistant", "", [
            {"type": "tool_use", "id": "toolu_01RELnotesEEEEEEEEEEEEE", "name": "Bash",
             "input": {"command": "gh pr list --state merged --search 'merged:>=2026-08-01' --json number,title,labels --limit 100", "description": "List merged PRs since the tag"}}]),
        ("assistant",
         "## v2.4 (draft)\n\n### Added\n- Cursor pagination on `/search` (#502)\n- Idempotency keys on `POST /refunds` (#509)\n- Usage CSV export in billing (#511)\n\n### Changed\n- CI now runs a Go 1.24/1.25 × Linux/macOS matrix (#497)\n- HTTP client retries use exponential backoff with jitter (#505)\n\n### Fixed\n- Upload handler no longer trusts the client filename or content type (#518)\n- Cart totals rounding with 3+ discounted items (#514)\n- Report buckets in non-whole-hour time zones (#507)\n\nWant me to write this to `CHANGELOG.md` under a `## v2.4` heading?",
         None),
        ("user", "Yes, and put the security fix first in its section.", None),
        ("assistant", "", [
            {"type": "tool_use", "id": "toolu_01RELeditFFFFFFFFFFFFFF", "name": "Edit",
             "input": {"file_path": f"{TOOL_HOME}/Projects/platform/CHANGELOG.md", "old_string": "# Changelog\n", "new_string": "# Changelog\n\n## v2.4\n…"}}]),
        ("assistant", "Written to `CHANGELOG.md`, security fix first under Fixed. Nothing is committed.", None),
    ]),
    ("Standup summary for Friday", "standup-bot", os.path.expanduser("~") + "/Projects/platform", "claude-sonnet-5", 2900, [
        ("user", "Post the standup summary for today.", None),
        ("assistant", "", [
            {"type": "tool_use", "id": "toolu_01STANDupGGGGGGGGGGGGGG", "name": "mcp__6b51c330-5c60-4d87-9a7c-d341455f2810__list_pull_requests",
             "input": {"owner": "acme", "repo": "gateway", "state": "open"}}]),
        ("assistant", "", [
            {"type": "tool_use", "id": "toolu_01STANDslackHHHHHHHHHHHH", "name": "mcp__d1a011b2-9e16-4b8b-9cb8-78f09c2d9431__send_message",
             "input": {"channel": "#eng-standup", "text": "*Since yesterday* — 6 PRs merged (gateway 3, payments 2, search 1). *Waiting on review*: #518 upload validation, #520 retry jitter. *CI*: nightly green."}}]),
        ("assistant", "Posted to #eng-standup: 6 merged, 2 waiting on review (#518, #520), nightly green.", None),
    ]),
    ("Explain the proration rules", "repo-librarian", os.path.expanduser("~") + "/Projects/billing", "claude-haiku-4-5-20251001", 4300, [
        ("user", "How is proration computed, and is a plan change on the 31st handled?", None),
        ("assistant", "", [
            {"type": "tool_use", "id": "toolu_01PROreadIIIIIIIIIIIIII", "name": "Read",
             "input": {"file_path": f"{TOOL_HOME}/Projects/billing/internal/billing/prorate.go"}}]),
        ("assistant",
         "`prorate.go:22` charges `remainingDays / daysInMonth × price` using the *current* month's length, so a change on the 31st credits 1/31 of the old plan and charges 1/31 of the new one — that part is fine. What it does not handle is a change that lands *after* the invoice for the month has been generated (`prorate.go:51` reads `invoice.Finalized` and returns early), so the credit is silently dropped. There is a `// TODO` on line 49 about exactly this.",
         None),
    ]),
]

for title, agent, cwd, model, mins_ago, msgs in CHATS:
    cid = str(uuid.uuid4())
    t = NOW - dt.timedelta(minutes=mins_ago)
    created = t
    conn.execute("""INSERT INTO chat_sessions (id, title, agent_slug, sdk_session_id, working_directory, model,
                    settings_profile_id, created_at, updated_at, is_favorite, permission_mode)
                    VALUES (?,?,?,?,?,?,?,?,?,?,?)""",
                 (cid, title, agent, str(uuid.uuid4()), cwd, model, "", gt(created), gt(created),
                  1 if title.startswith("Review") else 0, ""))
    tin = tout = tcc = tcr = 0
    for role, content, blocks in msgs:
        blocks_json = json.dumps(blocks, separators=(",", ":")) if blocks else "[]"
        conn.execute("INSERT INTO chat_messages (session_id, role, content, blocks, timestamp) VALUES (?,?,?,?,?)",
                     (cid, role, content, blocks_json, gt(t)))
        t += dt.timedelta(seconds=random.uniform(4, 40))
        if role == "assistant":
            tin += random.randint(3, 12); tout += random.randint(80, 600)
            tcc += random.randint(500, 9000); tcr += random.randint(15000, 60000)
    conn.execute("""UPDATE chat_sessions SET total_input_tokens=?, total_output_tokens=?, total_cache_creation_tokens=?,
                    total_cache_read_tokens=?, updated_at=? WHERE id=?""", (tin, tout, tcc, tcr, gt(t), cid))

# job history: a few runs for the active tasks
tasks = conn.execute("SELECT id, name, agent_slug, prompt FROM scheduled_tasks WHERE status='active'").fetchall()
runs = []
for tid, name, agent, prompt in tasks:
    for k in range(1, 6):
        started = NOW - dt.timedelta(days=k, hours=random.uniform(0, 3))
        dur = random.randint(40_000, 420_000)
        status = "success" if random.random() > 0.12 else "failed"
        err = "" if status == "success" else "agent run timed out after 15 minutes"
        runs.append((str(uuid.uuid4()), tid, name, agent, status, gt(started),
                     gt(started + dt.timedelta(milliseconds=dur)), dur, "", "claude-sonnet-5",
                     prompt[:80], err, random.randint(20, 400), random.randint(400, 4000),
                     random.randint(2000, 20000), random.randint(40000, 300000),
                     "" if status != "success" else "Done — see the linked chat for the full transcript."))
conn.executemany("""INSERT INTO job_history (id, task_id, task_name, agent_slug, status, started_at, finished_at,
                    duration_ms, chat_session_id, model, prompt_preview, error_message, total_input_tokens,
                    total_output_tokens, total_cache_creation_tokens, total_cache_read_tokens, response_text)
                    VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)""", runs)
for tid, name, agent, prompt in tasks:
    last = conn.execute("SELECT started_at, status FROM job_history WHERE task_id=? ORDER BY started_at DESC LIMIT 1", (tid,)).fetchone()
    conn.execute("UPDATE scheduled_tasks SET run_count=?, last_run_at=?, last_run_status=? WHERE id=?",
                 (5, last[0], last[1], tid))
conn.commit()
print("chats:", conn.execute("select count(*) from chat_sessions").fetchone()[0],
      "messages:", conn.execute("select count(*) from chat_messages").fetchone()[0],
      "jobs:", conn.execute("select count(*) from job_history").fetchone()[0])
