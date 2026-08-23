#!/usr/bin/env python3
"""Generate a synthetic Claude Code session corpus for screenshots.

Writes <fakehome>/.claude/projects/<encoded>/<session>.jsonl in the shape Claude
Code 2.1.x writes, plus sub-agent transcripts under <session>/subagents/. Nothing
here is real: projects, prompts, branches, PR numbers and token figures are all
invented. The project directories named in `cwd` must exist on disk for the
scanner to decode them (see decode_project_path), so `make_dirs.sh` creates them
as empty directories for the duration of the shoot.
"""
import json, os, random, sys, uuid, datetime as dt, shutil

random.seed(20260823)

FAKEHOME = sys.argv[1]
REAL_HOME = os.path.expanduser("~")  # the real home: fake project dirs live under ~/Projects
TOOL_HOME = "/home/dev"  # what tool inputs say; never resolved against disk, so it can be anonymous
NOW = dt.datetime(2026, 8, 23, 12, 30, tzinfo=dt.timezone.utc)
VERSION = "2.1.226"

PROJECTS = [
    # name, weight, branches
    ("auth",          4, ["main", "feat/passkeys", "fix/session-refresh", "chore/deps"]),
    ("gateway",       4, ["main", "feat/rate-limits", "develop", "fix/retry-storm"]),
    ("notifications", 3, ["main", "feat/webhook-retries", "fix/webhook-signature"]),
    ("payments",      3, ["main", "feat/refund-idempotency", "release/2.4"]),
    ("search",        3, ["main", "feat/cursor-pagination", "chore/reindex"]),
    ("storefront",    2, ["main", "release/2026-08", "fix/cart-totals"]),
    ("billing",       2, ["main", "feat/usage-export", "fix/proration"]),
    ("platform",      2, ["main", "chore/ci-matrix", "feat/otel"]),
    ("mobile-api",    1, ["main", "feat/push-badges"]),
    ("handbook",     1, ["main", "chore/link-check"]),
]

TITLES = [
    ("Add pagination to the search endpoint",
     "Add cursor-based pagination to the /search endpoint. Keep the existing response shape, add `next_cursor`, and make sure the integration tests cover an empty last page."),
    ("Split the monolith auth handler",
     "The auth handler in internal/http/auth.go does login, refresh and logout in one function. Split it into three handlers with shared helpers and keep behaviour identical."),
    ("Investigate p99 latency spike on /orders",
     "Since yesterday p99 on GET /orders doubled. Look at the query plan, the recent migrations and the cache layer and tell me what changed before touching anything."),
    ("Cut the v2.4 release",
     "Cut the v2.4 release: bump versions, write the changelog from merged PRs since v2.3, tag it and open the release PR."),
    ("Add idempotency keys to the refund endpoint",
     "Refunds can be double-submitted when the client retries. Add an Idempotency-Key header to POST /refunds backed by the existing keys table."),
    ("Convert the CI pipeline to matrix builds",
     "Our CI runs the test suite once on one Go version. Convert it to a matrix over 1.24 and 1.25 on Linux and macOS without slowing the fast path."),
    ("Fix flaky checkout integration test",
     "TestCheckoutFlow fails about one run in ten with a timeout. Find the race and fix it properly, no sleeps."),
    ("Harden the file upload validation",
     "Review the upload handler for anything that trusts the client-supplied filename or content type, and fix what you find."),
    ("Reproduce the reported timezone bug",
     "A customer in Asia/Kathmandu sees report buckets shifted by 15 minutes. Reproduce it with a test first, then fix."),
    ("Add rate limiting to the public API",
     "Add per-key rate limiting to the public API using the token bucket in pkg/ratelimit. 600 requests per minute, 429 with Retry-After."),
    ("Improve error messages on signup",
     "Signup errors all say 'invalid request'. Make each validation failure say what was wrong without leaking whether an email exists."),
    ("Trace a memory leak in the consumer",
     "The queue consumer's RSS grows 40 MB an hour. Take a heap profile against the staging replay and find the leak."),
    ("Migrate webhook retries to the outbox",
     "Webhook retries are fired from the request goroutine. Move them onto the outbox table so a crash does not lose them."),
    ("Write the runbook for the payments on-call",
     "Write docs/runbooks/payments.md covering the three alerts we actually get paged for, with the exact commands to run."),
    ("Upgrade to Go 1.25 and fix what breaks",
     "Bump go.mod to 1.25, run the full suite, and fix whatever the new vet checks and the loopvar change surface."),
    ("Review PR #482 for security issues",
     "Review pull request #482 (the new OAuth callback) for security problems only. Report, do not change anything."),
    ("Add a dark theme to the storefront",
     "Add a dark theme toggled by prefers-color-scheme. Tokens first, then components; do not hand-edit colours in components."),
    ("Explain the proration algorithm",
     "Explain how proration is computed in billing/prorate.go and whether it handles a plan change on the last day of a 31-day month."),
    ("Backfill missing invoice numbers",
     "About 300 invoices from July have a null number. Write a one-off migration that assigns them in created_at order, and a test."),
    ("Add OpenTelemetry spans to the gateway",
     "Instrument the gateway's request path with OTel spans: one per middleware and one for the upstream call. Keep attribute names consistent with the platform repo."),
    ("Triage the failing nightly build",
     "The nightly build has been red for three days. Find out why and either fix it or open an issue with the exact failure."),
    ("Implement passkey login",
     "Implement WebAuthn passkey login next to the password flow. Registration and authentication, with the challenge stored server-side."),
    ("Deduplicate push notifications",
     "Users get the same push twice when two workers pick up the same event. Add a dedupe key and a test that proves it."),
    ("Speed up the reindex job",
     "The full reindex takes 40 minutes. Profile it and get it under 10 without changing the index shape."),
    ("Add usage export to billing",
     "Add a CSV export of metered usage per customer per day, streamed rather than buffered, behind the existing admin auth."),
    ("Fix the cart total rounding bug",
     "Cart totals are off by a cent when a discount is applied to three or more items. Find the rounding order problem and fix it."),
    ("Write release notes for 2026-08",
     "Draft the August release notes from the merged PRs. Group by area, one line each, link the PR."),
    ("Clean up dead feature flags",
     "List every feature flag that has been fully on or off for more than 90 days, then remove the ones that are fully on."),
    ("Add retry with jitter to the HTTP client",
     "pkg/httpclient retries immediately and hammers upstreams. Add exponential backoff with full jitter and honour Retry-After."),
    ("Investigate the retry storm incident",
     "Read the incident timeline in docs/incidents/2026-08-14.md and find the code path that produced the retry storm."),
    ("Document the public API pagination",
     "Document how pagination works on every list endpoint in docs/api/pagination.md, with a worked example."),
    ("Audit third-party scripts on checkout",
     "List every third-party script loaded on the checkout page, what it does, and which ones we can drop."),
    ("Port the email templates to MJML",
     "Port the four transactional email templates to MJML and keep the rendered HTML byte-for-byte where possible."),
    ("Add a health endpoint with dependency checks",
     "Add GET /healthz that reports the database, the queue and the cache, each with its own status and latency."),
    ("Reduce Docker image size",
     "The API image is 1.2 GB. Get it under 100 MB with a multi-stage build without losing the debug tools we actually use."),
    ("Find why the link checker is slow",
     "The docs link checker takes 25 minutes. Find out where the time goes and parallelise it safely."),
    ("Add push badge counts",
     "Include an unread badge count in every push payload so the app icon stays accurate."),
    ("Make the webhook signature constant-time",
     "Webhook signature comparison uses ==. Switch to a constant-time compare and add a test that fails with the old code."),
    ("Write integration tests for refunds",
     "Write integration tests for the refund flow against the Stripe mock: full, partial, and a refund on an already-refunded charge."),
    ("Summarise open issues by area",
     "Pull the open issues with gh, group them by area label and give me a one-paragraph summary per area."),
]

MODELS = [
    ("claude-opus-5", 46),
    ("claude-sonnet-5", 38),
    ("claude-opus-4-1-20250805", 9),
    ("claude-haiku-4-5-20251001", 7),
]
SUBAGENT_MODELS = [("claude-sonnet-5", 60), ("claude-haiku-4-5-20251001", 40)]
PERMISSION_MODES = [("bypassPermissions", 45), ("default", 30), ("acceptEdits", 15), ("plan", 10)]
EFFORTS = [("high", 55), ("medium", 35), ("low", 10)]

TOOLS = [
    ("Read", 22), ("Edit", 14), ("Bash", 18), ("Grep", 12), ("Glob", 6), ("Write", 4),
    ("TodoWrite", 3), ("WebFetch", 2), ("Skill", 4), ("Agent", 3),
    ("mcp__github__list_pull_requests", 2), ("mcp__github__get_issue", 2),
    ("mcp__github__create_issue", 1), ("mcp__slack__send_message", 1),
    ("mcp__jira__search_issues", 1), ("mcp__local-tools__current_time", 1),
]
SKILLS = [("pr-reviewer", None), ("engineering", None), ("security-reviewer", None),
          ("commit", "git-tools"), ("release-notes", "release-tools"), ("local-verify", None)]
AGENT_TYPES = [("Explore", "Map the request path through the gateway"),
               ("general-purpose", "Find every caller of the old handler"),
               ("code-reviewer", "Review the diff for correctness and security"),
               ("Plan", "Plan the migration in reversible steps"),
               ("Explore", "Locate the rounding logic and its tests")]
FILES = ["internal/http/auth.go", "internal/http/orders.go", "pkg/ratelimit/bucket.go",
         "internal/search/cursor.go", "cmd/api/main.go", "internal/billing/prorate.go",
         "internal/webhooks/retry.go", "internal/store/migrations/0042_invoice_numbers.sql",
         "web/src/components/Cart.tsx", "web/src/styles/tokens.css", "Makefile",
         ".github/workflows/ci.yml", "docs/api/pagination.md", "internal/queue/consumer.go",
         "internal/payments/refund.go", "internal/notify/push.go", "go.mod", "README.md"]
CMDS = ["go test ./...", "go build ./...", "go vet ./...", "git status", "git diff --stat",
        "make lint", "gh pr list --limit 20", "go test -run TestCheckoutFlow -count=20 ./internal/checkout/",
        "npm run build", "npm test -- --watch=false", "git log --oneline -20",
        "docker build -t api:dev .", "go tool pprof -top heap.out", "gh issue list --label area/payments"]
RESPONSES = [
    "I'll start by reading the handler and its tests to see what the current behaviour is.",
    "The test exercises the happy path only. Let me look at how the retry is scheduled.",
    "Found it. The comparison runs before the body is fully read, so a slow client can race it.",
    "Let me check how the other list endpoints do this so the shape stays consistent.",
    "The migration is straightforward; the interesting part is keeping the ordering stable.",
    "Running the suite to make sure nothing else depended on the old behaviour.",
    "Two callers still used the old signature. Updated both and re-ran the tests.",
    "That reproduces it. The bucket boundary is computed in UTC and then shifted, which is the wrong order.",
]
FINALS = [
    "Done. The handler now validates the filename against an allowlist, rejects anything with a path separator, and the content type is sniffed rather than trusted. Three new tests cover the cases that used to pass.",
    "Summary of what changed:\n\n- `internal/search/cursor.go`: opaque cursor encoding the sort key and the row key\n- `internal/http/search.go`: `next_cursor` on every page, `400` on a cursor/sort mismatch\n- tests for the empty last page and a cursor minted under a different sort\n\nThe response shape is unchanged for callers that ignore the new field.",
    "The leak is in the consumer's per-message tracer: spans were started and never ended when the handler returned early. Fixed by deferring `span.End()` at the top of `handle`. RSS is flat over a 30-minute replay now.",
    "All green. 2 files changed, 41 insertions, 12 deletions. The PR is open as #512 with the changelog in the description.",
    "I would not merge this yet. The callback trusts `state` from the query string without comparing it to the session; that is an open redirect plus a login CSRF. Everything else looks fine. Details are in the review comments.",
    "The timezone bug is fixed and pinned by `TestBucketsKathmandu`. The cause was bucketing in UTC and converting afterwards; buckets are now built in the request's zone.",
    "Finished. The matrix runs four jobs; the fast path (Linux, 1.25) still gates the PR and the rest are required only for release branches.",
]
USER_FOLLOWUPS = [
    "Looks good. Can you also add a test for the error path?",
    "Ship it. Open the PR and link the issue.",
    "Hmm, I think the cursor should also encode the sort direction. Can you handle that?",
    "Before you change anything: what else calls this?",
    "Yes, go ahead.",
    "Please keep the old function as a deprecated wrapper for one release.",
    "What would this cost in the worst case? Give me the numbers.",
    "Run the full suite again and then commit.",
    "Can you explain the second option in more detail?",
    "No, leave the migration out of this PR. Just the code change.",
]


def pick(weighted):
    total = sum(w for _, w in weighted)
    r = random.uniform(0, total)
    for v, w in weighted:
        r -= w
        if r <= 0:
            return v
    return weighted[-1][0]


def uid():
    return str(uuid.uuid4())


def ts(t):
    return t.strftime("%Y-%m-%dT%H:%M:%S.") + f"{t.microsecond // 1000:03d}Z"


def toolu():
    return "toolu_01" + "".join(random.choice("ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz0123456789") for _ in range(22))


def msgid():
    return "msg_01" + "".join(random.choice("ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz0123456789") for _ in range(22))


def tool_input(name, project):
    f = random.choice(FILES)
    if name == "Read":
        return {"file_path": f"{TOOL_HOME}/Projects/{project}/{f}"}
    if name == "Edit":
        return {"file_path": f"{TOOL_HOME}/Projects/{project}/{f}",
                "old_string": "if sig == expected {", "new_string": "if subtle.ConstantTimeCompare([]byte(sig), []byte(expected)) == 1 {"}
    if name == "Write":
        return {"file_path": f"{TOOL_HOME}/Projects/{project}/{f}", "content": "package main\n"}
    if name == "Bash":
        return {"command": random.choice(CMDS), "description": "Run the checks"}
    if name == "Grep":
        return {"pattern": random.choice(["func handle", "Retry-After", "next_cursor", "time.UTC", "ConstantTimeCompare"]),
                "path": f"{TOOL_HOME}/Projects/{project}", "output_mode": "content"}
    if name == "Glob":
        return {"pattern": random.choice(["**/*_test.go", "internal/**/*.go", "web/src/**/*.tsx"])}
    if name == "TodoWrite":
        return {"todos": [{"content": "Read the handler", "status": "completed", "activeForm": "Reading the handler"},
                          {"content": "Write the failing test", "status": "in_progress", "activeForm": "Writing the failing test"},
                          {"content": "Fix and re-run", "status": "pending", "activeForm": "Fixing and re-running"}]}
    if name == "WebFetch":
        return {"url": "https://pkg.go.dev/crypto/subtle", "prompt": "How is ConstantTimeCompare meant to be used?"}
    if name == "Skill":
        return {"skill": random.choice(SKILLS)[0]}
    if name == "Agent":
        t, d = random.choice(AGENT_TYPES)
        return {"subagent_type": t, "description": d, "prompt": d + ". Report file paths and line numbers."}
    if name.startswith("mcp__github__"):
        return {"owner": "acme", "repo": project, **({"state": "open"} if "list" in name else {"issue_number": random.randint(100, 600)})}
    if name == "mcp__slack__send_message":
        return {"channel": "#eng-" + project, "text": "Deploy of " + project + " 2.4 starting in 10 minutes."}
    if name == "mcp__jira__search_issues":
        return {"jql": f'project = {project.upper()[:4]} AND status = "In Progress"', "max_results": 20}
    return {"timezone": "Europe/Berlin"}


def tool_result_text(name, is_error):
    if is_error:
        return random.choice(["Exit code 1\nFAIL\tinternal/checkout\t2.341s", "File does not exist.",
                              "Error: old_string not found in file", "<tool_use_error>File has not been read yet. Read it first before writing to it.</tool_use_error>"])
    if name == "Bash":
        return random.choice(["ok  \tinternal/http\t0.412s\nok  \tinternal/search\t1.203s\nok  \tpkg/ratelimit\t0.088s",
                              "On branch main\nnothing to commit, working tree clean",
                              " 3 files changed, 58 insertions(+), 14 deletions(-)", ""])
    if name == "Read":
        return "     1\tpackage http\n     2\t\n     3\timport (\n     4\t\t\"net/http\"\n     5\t)\n"
    if name == "Grep":
        return "internal/http/auth.go:88:func handleLogin(w http.ResponseWriter, r *http.Request) {\ninternal/http/auth.go:141:func handleRefresh(w http.ResponseWriter, r *http.Request) {"
    if name == "Edit":
        return "The file has been updated successfully."
    if name == "Agent":
        return "Found 3 call sites: internal/http/router.go:42, internal/http/router.go:57, cmd/api/main.go:118. The router ones are the only live paths; main.go is behind a feature flag that has been off since 2025."
    return "OK"


class Session:
    def __init__(self, start, project, branch, title, prompt, model, perm):
        self.id = uid()
        self.start = start
        self.t = start
        self.project = project
        self.cwd = f"{REAL_HOME}/Projects/{project}"
        self.branch = branch
        self.title = title
        self.prompt = prompt
        self.model = model
        self.perm = perm
        self.lines = []
        self.subagents = []  # (agent_id, lines, meta)
        self.last_uuid = None
        self.cache = random.randint(12000, 30000)
        self.turn_events = 0

    def base(self, extra=None):
        d = {"parentUuid": self.last_uuid, "isSidechain": False}
        return d

    def common(self):
        return {"userType": "external", "entrypoint": "cli", "cwd": self.cwd,
                "sessionId": self.id, "version": VERSION, "gitBranch": self.branch}

    def advance(self, lo, hi):
        self.t += dt.timedelta(seconds=random.uniform(lo, hi))

    def emit(self, ev):
        self.lines.append(json.dumps(ev, ensure_ascii=False))

    def user_prompt(self, text):
        u = uid()
        ev = {"parentUuid": self.last_uuid, "isSidechain": False, "promptId": uid(), "type": "user",
              "message": {"role": "user", "content": text}, "uuid": u, "timestamp": ts(self.t),
              "permissionMode": self.perm, "origin": {"kind": "human"}, "promptSource": "typed",
              **self.common()}
        self.emit(ev)
        self.last_uuid = u

    def usage(self, out):
        inp = random.randint(3, 12) if random.random() < 0.7 else random.randint(400, 2500)
        create = random.choice([0, 0, random.randint(300, 4000), random.randint(4000, 12000)])
        if self.turn_events == 0:
            create = random.randint(8000, 26000)
        read = self.cache
        self.cache += create + random.randint(200, 900)
        five, one = (create, 0) if random.random() < 0.7 else (0, create)
        return {"input_tokens": inp, "cache_creation_input_tokens": create, "cache_read_input_tokens": read,
                "output_tokens": out, "server_tool_use": {"web_search_requests": 0, "web_fetch_requests": 0},
                "service_tier": "standard",
                "cache_creation": {"ephemeral_5m_input_tokens": five, "ephemeral_1h_input_tokens": one},
                "inference_geo": "not_available", "speed": "standard"}

    def assistant(self, blocks, attrib, effort, out=None):
        mid = msgid()
        rid = "req_01" + uuid.uuid4().hex[:22]
        out = out or random.randint(60, 900)
        usage = self.usage(out)
        for b in blocks:
            u = uid()
            ev = {"parentUuid": self.last_uuid, "isSidechain": False,
                  "message": {"model": self.model, "id": mid, "type": "message", "role": "assistant",
                              "content": [b], "stop_reason": "tool_use" if b["type"] == "tool_use" else "end_turn",
                              "stop_sequence": None, "usage": usage},
                  "requestId": rid, "type": "assistant", "uuid": u, "timestamp": ts(self.t), "effort": effort,
                  **attrib, **self.common()}
            self.emit(ev)
            self.last_uuid = u
            self.turn_events += 1
            self.advance(0.4, 3.0)
        return mid

    def tool_result(self, tool_id, name, is_error, text=None):
        u = uid()
        ev = {"parentUuid": self.last_uuid, "isSidechain": False, "type": "user",
              "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tool_id,
                                                        "content": text if text is not None else tool_result_text(name, is_error),
                                                        **({"is_error": True} if is_error else {})}]},
              "uuid": u, "timestamp": ts(self.t), "sourceToolAssistantUUID": self.last_uuid,
              "toolUseResult": {"ok": not is_error}, **self.common()}
        self.emit(ev)
        self.last_uuid = u

    def subagent(self, tool_id, agent_type, description, effort):
        agent_id = uuid.uuid4().hex[:17]
        model = pick(SUBAGENT_MODELS)
        lines = []
        last = None
        cache = random.randint(5000, 15000)
        t = self.t

        def common():
            return {"userType": "external", "entrypoint": "cli", "cwd": self.cwd, "sessionId": self.id,
                    "version": VERSION, "gitBranch": self.branch}

        u = uid()
        lines.append(json.dumps({"parentUuid": None, "isSidechain": True, "agentId": agent_id, "type": "user",
                                 "message": {"role": "user", "content": description + ". Report file paths and line numbers."},
                                 "uuid": u, "timestamp": ts(t), **common()}))
        last = u
        steps = random.randint(3, 14)
        for i in range(steps + 1):
            t += dt.timedelta(seconds=random.uniform(1, 6))
            final = i == steps
            name = pick([("Read", 40), ("Grep", 30), ("Glob", 15), ("Bash", 15)])
            tid = toolu()
            block = ({"type": "text", "text": tool_result_text("Agent", False)} if final
                     else {"type": "tool_use", "id": tid, "name": name, "input": tool_input(name, self.project), "caller": {"type": "direct"}})
            create = random.choice([0, 0, random.randint(200, 3000)])
            usage = {"input_tokens": random.randint(3, 10), "cache_creation_input_tokens": create,
                     "cache_read_input_tokens": cache, "output_tokens": random.randint(40, 400),
                     "cache_creation": {"ephemeral_5m_input_tokens": create, "ephemeral_1h_input_tokens": 0},
                     "service_tier": "standard"}
            cache += create + random.randint(100, 600)
            u = uid()
            lines.append(json.dumps({"parentUuid": last, "isSidechain": True, "agentId": agent_id,
                                     "message": {"model": model, "id": msgid(), "type": "message", "role": "assistant",
                                                 "content": [block], "stop_reason": "end_turn" if final else "tool_use",
                                                 "stop_sequence": None, "usage": usage},
                                     "requestId": "req_01" + uuid.uuid4().hex[:22], "type": "assistant", "uuid": u,
                                     "timestamp": ts(t), "effort": effort, "attributionAgent": agent_type, **common()}))
            last = u
            if not final:
                t += dt.timedelta(seconds=random.uniform(0.2, 4))
                u = uid()
                lines.append(json.dumps({"parentUuid": last, "isSidechain": True, "agentId": agent_id, "type": "user",
                                         "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tid,
                                                                                   "content": tool_result_text(name, False)}]},
                                         "uuid": u, "timestamp": ts(t), "sourceToolAssistantUUID": last,
                                         "toolUseResult": {"ok": True}, **common()}))
                last = u
        self.t = t + dt.timedelta(seconds=random.uniform(0.5, 2))
        meta = {"agentType": agent_type, "description": description, "toolUseId": tool_id, "spawnDepth": 1}
        self.subagents.append((agent_id, lines, meta))

    def turn(self, steps, final_text):
        self.turn_events = 0
        effort = pick(EFFORTS)
        skill = None  # (name, plugin)
        t0 = self.t
        for i in range(steps):
            attrib = {}
            if skill:
                attrib["attributionSkill"] = skill[0]
                if skill[1]:
                    attrib["attributionPlugin"] = skill[1]
            name = pick(TOOLS)
            blocks = []
            if random.random() < 0.3:
                blocks.append({"type": "thinking", "thinking": "", "signature": "Eo" + uuid.uuid4().hex})
            if random.random() < 0.35:
                blocks.append({"type": "text", "text": random.choice(RESPONSES)})
            tid = toolu()
            inp = tool_input(name, self.project)
            blocks.append({"type": "tool_use", "id": tid, "name": name, "input": inp, "caller": {"type": "direct"}})
            self.assistant(blocks, attrib, effort)
            if name == "Skill":
                sk = next((s for s in SKILLS if s[0] == inp["skill"]), SKILLS[0])
                skill = sk
            is_error = random.random() < 0.035
            if name == "Agent" and not is_error:
                self.subagent(tid, inp["subagent_type"], inp["description"], effort)
            elif name == "Bash":
                self.advance(0.5, 40)
            else:
                self.advance(0.1, 3)
            self.tool_result(tid, name, is_error)
            self.advance(1.5, 6)
        attrib = {}
        if skill:
            attrib["attributionSkill"] = skill[0]
            if skill[1]:
                attrib["attributionPlugin"] = skill[1]
        self.assistant([{"type": "text", "text": final_text}], attrib, effort, out=random.randint(150, 700))
        # turn_duration system event
        u = uid()
        self.emit(json.dumps if False else {"parentUuid": self.last_uuid, "isSidechain": False, "type": "system",
                                            "subtype": "turn_duration", "durationMs": int((self.t - t0).total_seconds() * 1000),
                                            "messageCount": len(self.lines), "timestamp": ts(self.t), "uuid": u,
                                            "isMeta": False, **self.common()})
        self.last_uuid = u

    def build(self):
        self.emit({"type": "permission-mode", "permissionMode": self.perm, "sessionId": self.id})
        self.emit({"type": "mode", "mode": "normal", "sessionId": self.id})
        self.user_prompt(self.prompt)
        self.emit({"type": "ai-title", "aiTitle": self.title, "sessionId": self.id})
        self.advance(2, 6)
        nturns = pick([(1, 30), (2, 30), (3, 20), (4, 12), (5, 5), (6, 3)])
        for n in range(nturns):
            steps = random.randint(3, 26) if n == 0 else random.randint(1, 14)
            self.turn(steps, random.choice(FINALS))
            self.emit({"type": "last-prompt", "lastPrompt": self.prompt if n == 0 else "…", "leafUuid": self.last_uuid, "sessionId": self.id})
            if random.random() < 0.22 and n == 0:
                self.emit({"type": "pr-link", "sessionId": self.id, "prNumber": random.randint(120, 980),
                           "prUrl": f"https://github.com/acme/{self.project}/pull/{random.randint(120, 980)}",
                           "prRepository": f"acme/{self.project}", "timestamp": ts(self.t)})
            if n < nturns - 1:
                gap = random.choice([random.uniform(20, 180), random.uniform(120, 600), random.uniform(600, 1500),
                                     random.uniform(1800, 9000)])
                self.t += dt.timedelta(seconds=gap)
                self.user_prompt(random.choice(USER_FOLLOWUPS))
                self.advance(2, 6)

    def write(self, root):
        enc = "-" + self.cwd.strip("/").replace("/", "-")
        d = os.path.join(root, enc)
        os.makedirs(d, exist_ok=True)
        path = os.path.join(d, self.id + ".jsonl")
        with open(path, "w") as f:
            f.write("\n".join(self.lines) + "\n")
        mt = self.t.timestamp()
        os.utime(path, (mt, mt))
        for agent_id, lines, meta in self.subagents:
            sd = os.path.join(d, self.id, "subagents")
            os.makedirs(sd, exist_ok=True)
            p = os.path.join(sd, f"agent-{agent_id}.jsonl")
            with open(p, "w") as f:
                f.write("\n".join(lines) + "\n")
            os.utime(p, (mt, mt))
            with open(os.path.join(sd, f"agent-{agent_id}.meta.json"), "w") as f:
                json.dump(meta, f)


# ─── The hero session: scripted, coherent, opened for the detail screenshots ──
HERO_TITLE = "Harden the file upload validation"


def hero_session():
    start = NOW - dt.timedelta(hours=3, minutes=12)
    s = Session(start, "gateway", "fix/upload-validation", HERO_TITLE,
                "Review the upload handler in internal/http/upload.go for anything that trusts the "
                "client-supplied filename or content type, and fix what you find. Keep the public "
                "response shape; add tests for every case you close.",
                "claude-opus-5", "acceptEdits")
    s.id = "7f3c2a10-4b6e-4d1a-9c2e-0f8e5d2a6b91"
    p = f"{TOOL_HOME}/Projects/gateway"
    s.emit({"type": "permission-mode", "permissionMode": s.perm, "sessionId": s.id})
    s.emit({"type": "mode", "mode": "normal", "sessionId": s.id})
    s.user_prompt(s.prompt)
    s.emit({"type": "ai-title", "aiTitle": HERO_TITLE, "sessionId": s.id})
    s.advance(2, 4)
    t0 = s.t

    def step(blocks, name=None, inp=None, result=None, is_error=False, effort="high", bash_secs=None):
        tid = toolu()
        bl = list(blocks)
        if name:
            bl.append({"type": "tool_use", "id": tid, "name": name, "input": inp, "caller": {"type": "direct"}})
        s.assistant(bl, {}, effort)
        if name:
            s.advance(*(bash_secs or (0.2, 2.5)))
            s.tool_result(tid, name, is_error, result)
            s.advance(1.5, 5)
        return tid

    step([{"type": "thinking", "thinking": "", "signature": "Eo" + uuid.uuid4().hex},
          {"type": "text", "text": "I'll start by reading the upload handler and its tests, then look at where the filename and content type actually come from."}],
         "Read", {"file_path": f"{p}/internal/http/upload.go"},
         "     1\tpackage http\n     2\t\n     3\tfunc (s *Server) handleUpload(w http.ResponseWriter, r *http.Request) {\n     4\t\tf, hdr, err := r.FormFile(\"file\")\n    ...\n    31\t\tname := filepath.Base(hdr.Filename)\n    32\t\tdst := filepath.Join(s.uploadDir, name)\n    33\t\tct := hdr.Header.Get(\"Content-Type\")\n")
    step([], "Read", {"file_path": f"{p}/internal/http/upload_test.go"},
         "     1\tpackage http\n     2\t\n     3\tfunc TestUploadHappyPath(t *testing.T) {\n")
    step([{"type": "text", "text": "Two things trust the client here: the destination is built from `hdr.Filename` with only `filepath.Base`, and the stored content type is whatever the multipart part declared. Let me check who else calls `saveUpload` before I change its signature."}],
         "Grep", {"pattern": "saveUpload\\(", "path": p, "output_mode": "content"},
         "internal/http/upload.go:40:func (s *Server) saveUpload(name, ct string, r io.Reader) (string, error) {\ninternal/http/upload.go:34:\tpath, err := s.saveUpload(name, ct, f)\ninternal/http/avatar.go:58:\tpath, err := s.saveUpload(hdr.Filename, \"image/png\", f)")
    # delegate the caller survey to a sub-agent
    tid = toolu()
    s.assistant([{"type": "tool_use", "id": tid, "name": "Agent",
                  "input": {"subagent_type": "Explore", "description": "Map every caller of saveUpload",
                            "prompt": "Find every caller of saveUpload in the gateway repo and report what each passes as the filename and content type, with file paths and line numbers."},
                  "caller": {"type": "direct"}}], {}, "high")
    s.subagent(tid, "Explore", "Map every caller of saveUpload", "medium")
    s.tool_result(tid, "Agent", False,
                  "Two callers:\n- internal/http/upload.go:34 — passes hdr.Filename (client-controlled) and the part's Content-Type header.\n- internal/http/avatar.go:58 — passes hdr.Filename and a hardcoded image/png.\nNothing else calls it; the old cmd/import path was deleted in #431.")
    s.advance(2, 4)
    step([{"type": "text", "text": "Only two callers, so I can tighten `saveUpload` itself rather than each handler. The fix: an allowlist of extensions, a generated file name on disk, and a sniffed content type."}],
         "Edit", {"file_path": f"{p}/internal/http/upload.go",
                  "old_string": "\tname := filepath.Base(hdr.Filename)\n\tdst := filepath.Join(s.uploadDir, name)\n\tct := hdr.Header.Get(\"Content-Type\")",
                  "new_string": "\text, err := sanitizeExtension(hdr.Filename)\n\tif err != nil {\n\t\twriteError(w, http.StatusBadRequest, err)\n\t\treturn\n\t}\n\tdst := filepath.Join(s.uploadDir, uuid.NewString()+ext)\n\tct, err := sniffContentType(f)"},
         "The file has been updated successfully.")
    step([], "Write", {"file_path": f"{p}/internal/http/upload_sanitize.go", "content": "package http\n\n// sanitizeExtension allows only a known set of extensions ...\n"},
         "File created successfully.")
    step([{"type": "text", "text": "Now the tests: a path-traversal name, a double extension, an empty filename, and a part whose declared type disagrees with its bytes."}],
         "Edit", {"file_path": f"{p}/internal/http/upload_test.go",
                  "old_string": "func TestUploadHappyPath(t *testing.T) {",
                  "new_string": "func TestUploadRejectsTraversal(t *testing.T) {\n\t// ...\n}\n\nfunc TestUploadSniffsContentType(t *testing.T) {\n\t// ...\n}\n\nfunc TestUploadHappyPath(t *testing.T) {"},
         "The file has been updated successfully.")
    step([], "Bash", {"command": "go test ./internal/http/ -run 'TestUpload' -count=1", "description": "Run the upload tests"},
         "--- FAIL: TestUploadSniffsContentType (0.00s)\n    upload_test.go:88: got content type \"application/octet-stream\", want \"image/png\"\nFAIL\nFAIL\tacme/gateway/internal/http\t0.412s", is_error=True, bash_secs=(3, 9))
    step([{"type": "text", "text": "The sniff reads the body after the handler has already consumed it. I'll peek the first 512 bytes and rewind before saving."}],
         "Edit", {"file_path": f"{p}/internal/http/upload_sanitize.go",
                  "old_string": "func sniffContentType(r io.Reader) (string, error) {",
                  "new_string": "func sniffContentType(r io.ReadSeeker) (string, error) {\n\tdefer r.Seek(0, io.SeekStart)"},
         "The file has been updated successfully.")
    step([], "Bash", {"command": "go test ./internal/http/ -count=1", "description": "Run the package tests"},
         "ok  \tacme/gateway/internal/http\t0.731s", bash_secs=(4, 10))
    step([], "Bash", {"command": "go vet ./... && make lint", "description": "Vet and lint"},
         "ok", bash_secs=(5, 14))
    s.assistant([{"type": "text", "text": "Done. Three things trusted the client and all three are closed:\n\n- **Filename** — `saveUpload` now allowlists the extension (`sanitizeExtension`) and stores the file under a generated name, so `../../etc/cron.d/x.png` and `report.pdf.exe` are both rejected and nothing client-supplied reaches the path.\n- **Content type** — sniffed from the first 512 bytes with `http.DetectContentType` and compared against the extension; a mismatch is a 400.\n- **The avatar handler** inherited the fix for free, since it goes through the same function.\n\nFour new tests cover traversal, double extension, empty name and a mismatched type. `go test ./internal/http/` and `make lint` are green."}],
                {}, "high", out=420)
    u = uid()
    s.emit({"parentUuid": s.last_uuid, "isSidechain": False, "type": "system", "subtype": "turn_duration",
            "durationMs": int((s.t - t0).total_seconds() * 1000), "messageCount": len(s.lines),
            "timestamp": ts(s.t), "uuid": u, "isMeta": False, **s.common()})
    s.last_uuid = u
    s.emit({"type": "last-prompt", "lastPrompt": s.prompt, "leafUuid": s.last_uuid, "sessionId": s.id})
    # second turn: ship it
    s.t += dt.timedelta(minutes=6, seconds=23)
    s.user_prompt("Looks right. Commit it on a branch and open the PR; link it to #512.")
    s.advance(2, 4)
    t1 = s.t
    step([{"type": "text", "text": "Committing on `fix/upload-validation` and opening the PR against main."}],
         "Bash", {"command": "git checkout -b fix/upload-validation && git add -A && git commit -m 'http: stop trusting client-supplied upload filename and content type'", "description": "Commit on a branch"},
         "Switched to a new branch 'fix/upload-validation'\n[fix/upload-validation 3e1f9c2] http: stop trusting client-supplied upload filename and content type\n 3 files changed, 141 insertions(+), 9 deletions(-)", bash_secs=(1, 3))
    step([], "Bash", {"command": "git push -u origin fix/upload-validation && gh pr create --fill --base main --body 'Closes #512'", "description": "Push and open the PR"},
         "https://github.com/acme/gateway/pull/518", bash_secs=(3, 8))
    s.emit({"type": "pr-link", "sessionId": s.id, "prNumber": 518, "prUrl": "https://github.com/acme/gateway/pull/518",
            "prRepository": "acme/gateway", "timestamp": ts(s.t)})
    s.assistant([{"type": "text", "text": "Opened [acme/gateway#518](https://github.com/acme/gateway/pull/518) from `fix/upload-validation`, linked to close #512. CI is running; I'll leave the merge to you."}],
                {}, "high", out=120)
    u = uid()
    s.emit({"parentUuid": s.last_uuid, "isSidechain": False, "type": "system", "subtype": "turn_duration",
            "durationMs": int((s.t - t1).total_seconds() * 1000), "messageCount": len(s.lines),
            "timestamp": ts(s.t), "uuid": u, "isMeta": False, **s.common()})
    s.emit({"type": "last-prompt", "lastPrompt": "Looks right. Commit it on a branch and open the PR; link it to #512.", "leafUuid": u, "sessionId": s.id})
    return s


def day_sessions(day):
    wd = day.weekday()
    if wd >= 5:
        return random.choice([1, 1, 2, 2, 3])
    return random.choice([3, 4, 5, 5, 6, 7, 8, 9])


def main():
    root = os.path.join(FAKEHOME, ".claude", "projects")
    if os.path.isdir(root):
        shutil.rmtree(root)
    os.makedirs(root)
    total = 0
    proj_weights = [(p, w) for p, w, _ in PROJECTS]
    branches = {p: b for p, _, b in PROJECTS}
    # Berlin is UTC+2 in summer; work hours 08-19 local => 06-17 UTC
    for back in range(0, 62):
        day = (NOW - dt.timedelta(days=back)).replace(hour=0, minute=0, second=0, microsecond=0)
        n = day_sessions(day)
        if back == 0:
            n = 4
        for _ in range(n):
            hour = random.choice([6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 12, 13, 13, 14, 14, 15, 16, 17, 19, 20])
            start = day + dt.timedelta(hours=hour, minutes=random.randint(0, 59), seconds=random.randint(0, 59))
            if start > NOW - dt.timedelta(minutes=35):
                start = NOW - dt.timedelta(hours=random.uniform(1, 6))
            project = pick(proj_weights)
            title, prompt = random.choice(TITLES)
            s = Session(start, project, random.choice(branches[project]), title, prompt, pick(MODELS), pick(PERMISSION_MODES))
            s.build()
            s.write(root)
            total += 1
    h = hero_session()
    h.write(root)
    total += 1
    # settings.json so the Claude settings surface has something to read
    os.makedirs(os.path.join(FAKEHOME, ".claude"), exist_ok=True)
    with open(os.path.join(FAKEHOME, ".claude", "settings.json"), "w") as f:
        json.dump({"permissions": {"allow": ["Bash(go test:*)", "Bash(npm run:*)"]},
                   "env": {"CLAUDE_CODE_ENABLE_TELEMETRY": "0"}}, f, indent=2)
    print("sessions:", total)


if __name__ == "__main__":
    main()
