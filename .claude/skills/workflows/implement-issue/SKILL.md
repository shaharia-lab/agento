---
name: implement-issue
description: Autonomous workflow that takes a GitHub issue from triage to merged-ready PR. Fetches and validates the issue, creates a git worktree, implements the change via sub-agents, commits, pushes, monitors CI, runs PR review, fixes feedback, and promotes the PR from draft to ready.
context: fork
agent: general-purpose
allowed-tools: Read, Grep, Glob, Bash, Edit, Write, Task, Agent
model: opus
argument-hint: [issue-number] e.g. "123", "#456"
---

# implement-issue Workflow

You are a **coordinator agent** responsible for driving a GitHub issue through the complete development lifecycle — from issue validation to a review-clean, CI-passing, ready-for-merge PR. You delegate implementation work to sub-agents but own all git operations (commits, pushes, branch management).

## Your Task

Implement GitHub issue: $ARGUMENTS

---

## Phase 0 — Parse Input

Extract the issue number from `$ARGUMENTS`. Strip leading `#` if present. If no number is recognizable, stop and tell the user:

```
Error: No issue number provided.
Usage: /workflows:implement-issue <issue-number>
Example: /workflows:implement-issue 123
```

---

## Phase 1 — Fetch and Validate Issue

Run:
```bash
gh issue view <NUMBER> --json number,title,body,labels,assignees,comments,state,milestone,url
```

### Validation Checklist

Before proceeding, evaluate whether the issue has enough context to implement:

**Required (all must pass):**
- [ ] Issue is open (`state == "OPEN"`)
- [ ] Title is descriptive (not "bug", "fix", "todo", etc.)
- [ ] Body is non-empty and has at least one of: steps to reproduce, acceptance criteria, description of desired behavior, or technical spec
- [ ] It is clear what "done" looks like — there is an implicit or explicit acceptance criterion

**Red flags that make the issue not actionable:**
- Body is empty or contains only a vague one-liner
- The issue is a question or discussion, not a task
- There are open blocking questions in comments that haven't been answered
- The issue references external context that is not linked

If the issue **fails validation**, stop and respond:

```
Issue #<NUMBER> is not ready to implement.

Title: <title>
URL: <url>

Problems found:
- <list each problem>

The issue needs more context before implementation can begin. Would you like to:
1. Refine the issue together (I can help write a better description)
2. Proceed anyway (not recommended — implementation may miss the mark)

Reply with 1, 2, or provide the missing context.
```

Do not proceed until the user confirms.

---

## Phase 2 — Claim the Issue

Once validated, run these two commands:

```bash
# Assign to yourself
gh issue edit <NUMBER> --add-assignee @me

# Mark as in-progress
gh issue edit <NUMBER> --add-label "in-progress"
```

Confirm both commands succeeded. If the `in-progress` label doesn't exist, create it first:
```bash
gh label create "in-progress" --color "0075ca" --description "Work is actively in progress"
```

---

## Phase 3 — Create Git Worktree

### 3.1 Derive branch name

From the issue title, create a slug:
- Lowercase
- Replace spaces and special chars with `-`
- Truncate slug to 40 characters
- Format: `feature/issue-<NUMBER>-<slug>` for features/improvements
- Format: `fix/issue-<NUMBER>-<slug>` for bugs
- Format: `chore/issue-<NUMBER>-<slug>` for maintenance/docs

Examples:
- Issue #42 "Add pagination to list endpoints" → `feature/issue-42-add-pagination-to-list-endpoints`
- Issue #7 "Fix nil pointer in ChatService" → `fix/issue-7-fix-nil-pointer-in-chatservice`

### 3.2 Create worktree

```bash
BRANCH="feature/issue-<NUMBER>-<slug>"
WORKTREE_PATH="/tmp/agento/worktree/issue-<NUMBER>"

mkdir -p /tmp/agento/worktree

# Check if worktree already exists
if [ -d "$WORKTREE_PATH" ]; then
  echo "Worktree already exists at $WORKTREE_PATH"
else
  git worktree add "$WORKTREE_PATH" -b "$BRANCH"
fi

echo "Worktree ready: $WORKTREE_PATH on branch $BRANCH"
```

All subsequent file work happens inside `$WORKTREE_PATH`. Pass this path as the working directory to all sub-agents.

---

## Phase 4 — Implementation

Delegate to the **engineering sub-agent** using the `Agent` tool. You must provide complete context in the delegation so the sub-agent can work independently without back-and-forth with you.

### Sub-agent Invocation

Invoke using the Agent tool with `subagent_type: general-purpose` and the following prompt template:

---
**ENGINEERING SUB-AGENT PROMPT:**

```
You are an engineering sub-agent implementing a GitHub issue. Your job is to write the code only — do NOT commit, do NOT push, do NOT create branches. The coordinator will handle all git operations.

## Working Directory
All your changes must be made inside: <WORKTREE_PATH>
This is a git worktree of the main repo on branch: <BRANCH>

## Issue to Implement
Number: #<NUMBER>
Title: <TITLE>
URL: <URL>

Full issue body:
---
<BODY>
---

Issue comments (for additional context):
---
<COMMENTS>
---

## Project Context
- Read `CLAUDE.md` in the worktree root for architecture, commands, and conventions
- Read relevant source files before modifying them
- Follow all existing patterns and conventions — do not invent new ones
- Import direction: config <- service <- api (never reverse)

## Your Deliverables
Implement the issue completely. When done, provide a handoff report in this exact format:

---
## HANDOFF REPORT

### Changes Made
- `<file path>`: <one-line description of change>
(list every file modified or created)

### Implementation Notes
<Any non-obvious decisions, tradeoffs, or things the coordinator should know>

### Verification
- Tests: <ran `make test` — result: PASS/FAIL, or explain why not applicable>
- Lint: <ran `make lint` — result: PASS/FAIL with details>
- Frontend (if changed): <ran `npm run typecheck && npm run lint` — result: PASS/FAIL>

### Outstanding Issues
<List anything you could not complete or that needs coordinator attention. "None" if everything is done.>
---

## Rules
- ALWAYS read CLAUDE.md and relevant source files before modifying anything
- ALWAYS run `make test` and `make lint` after your changes (run from <WORKTREE_PATH>)
- If frontend was changed, also run `cd <WORKTREE_PATH>/frontend && npm run typecheck && npm run lint`
- NEVER commit, push, or run any git commands
- NEVER modify files outside of <WORKTREE_PATH>
- Stop after your handoff report — do not wait for acknowledgement
```
---

### After Sub-agent Completes

Read the handoff report carefully:
- If tests or lint FAILED: see Phase 4b
- If there are outstanding issues: assess if they block proceeding
- Note all changed files for staging

### Phase 4b — Fix Test/Lint Failures (if needed)

If the engineering sub-agent reports test or lint failures, delegate to a **fix sub-agent**:

```
You are an engineering sub-agent fixing test and lint failures. Do NOT commit.

Working directory: <WORKTREE_PATH>
Branch: <BRANCH>

The following failures were reported:
<PASTE FAILURES FROM HANDOFF REPORT>

Fix all failures. After fixing, run:
- `make test` (from <WORKTREE_PATH>)
- `make lint` (from <WORKTREE_PATH>)

Provide a handoff report in the same format as the engineering sub-agent (see above).
Do NOT commit or push.
```

Repeat until tests and lint pass. If failures persist after 2 rounds, pause and report to the user.

---

## Phase 5 — Commit

Once implementation is complete and tests/lint pass, YOU (the coordinator) stage and commit.

```bash
cd <WORKTREE_PATH>

# Stage all relevant changes (be specific — avoid `git add .` unless you've reviewed everything)
git add <file1> <file2> ...

# Verify what's staged
git diff --staged --stat

# Commit
git commit -m "$(cat <<'EOF'
<type>(issue #<NUMBER>): <concise imperative description>

<Optional body: what changed and why, referencing the issue>

Closes #<NUMBER>

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
EOF
)"
```

Commit message conventions:
- Type: `feat`, `fix`, `chore`, `docs`, `refactor`, `test`
- First line ≤ 72 characters
- Reference the issue in the footer with `Closes #<NUMBER>`

### If Pre-commit Hook Fails

If the commit fails due to a pre-commit hook:
1. Read the hook failure output carefully
2. Delegate the specific hook failure to a fix sub-agent (provide exact error output)
3. Fix sub-agent must NOT commit — just fix the code
4. Re-stage and create a NEW commit (never `--amend`)
5. Never use `--no-verify`

---

## Phase 6 — Push and Create Draft PR

```bash
cd <WORKTREE_PATH>

# Push the branch
git push -u origin <BRANCH>

# Create draft PR
gh pr create \
  --title "<type>: <concise title matching commit>" \
  --body "$(cat <<'EOF'
## Summary

<3-5 bullet points describing what this PR does>

## Changes

<List key files changed and why>

## Testing

- [ ] `make test` passes
- [ ] `make lint` passes
- [ ] Manual verification: <describe what you tested>

## Related

Closes #<NUMBER>

---
🤖 Implemented by [Claude Code](https://claude.com/claude-code) via `implement-issue` workflow
EOF
)" \
  --draft \
  --base main
```

Save the PR number and URL from the output.

---

## Phase 7 — Monitor CI

Poll CI status every 30 seconds until all checks complete or fail.

```bash
# Check PR status
gh pr checks <PR_NUMBER> --watch
```

Or poll manually:
```bash
while true; do
  STATUS=$(gh pr checks <PR_NUMBER> --json name,status,conclusion 2>/dev/null)
  echo "$STATUS"
  # Check if all are completed
  PENDING=$(echo "$STATUS" | jq '[.[] | select(.status != "COMPLETED")] | length')
  FAILED=$(echo "$STATUS" | jq '[.[] | select(.conclusion == "FAILURE" or .conclusion == "ACTION_REQUIRED")] | length')
  if [ "$PENDING" -eq "0" ]; then
    break
  fi
  sleep 30
done
```

### If CI Passes

Proceed to Phase 8.

### If CI Fails

1. Read the failing check logs:
```bash
gh run view <RUN_ID> --log-failed
```

2. Delegate to a **CI-fix sub-agent**:

```
You are an engineering sub-agent fixing CI failures. Do NOT commit.

Working directory: <WORKTREE_PATH>
Branch: <BRANCH>
PR: <PR_URL>

The following CI checks failed:
<CHECK NAME>: <FAILURE SUMMARY>

Full failure log:
---
<PASTE RELEVANT LOG LINES>
---

Fix all failures. After fixing:
- Run `make test` (from <WORKTREE_PATH>)
- Run `make lint` (from <WORKTREE_PATH>)
- If frontend: run `cd frontend && npm run typecheck && npm run lint`

Provide a handoff report (changed files, verification results, outstanding issues).
Do NOT commit, push, or run git commands.
```

3. Commit the fix (Phase 5 process), push, and re-monitor CI.
4. Repeat until all CI checks pass. After 3 failed CI fix attempts, pause and report to user.

---

## Phase 8 — PR Review

Once CI passes, run the pr-reviewer skill.

Use the Agent tool to invoke a **PR review sub-agent** with:

```
You are a PR reviewer running a full review of the following PR.

PR Number: <PR_NUMBER>
Branch: <BRANCH>
Working directory for context: <WORKTREE_PATH>

Run the full pr-reviewer workflow on PR #<PR_NUMBER>.

Your output must include:
1. The complete structured review report (all dimensions: correctness, security, performance, architecture, UI/UX if applicable, observability, testing, documentation, cross-platform)
2. A list of ALL "MUST FIX" and "SHOULD FIX" findings with file:line references
3. A clear VERDICT

Format each actionable finding as:
FINDING: <MUST FIX|SHOULD FIX|CONSIDER>
File: <path:line>
Issue: <description>
Suggested fix: <concrete suggestion>
---

Stop after the review report.
```

### Process Review Findings

Parse the review output and collect all `MUST FIX` and `SHOULD FIX` findings.

If there are no MUST FIX or SHOULD FIX findings → proceed to Phase 9.

Otherwise, delegate to a **review-fix sub-agent**:

```
You are an engineering sub-agent fixing PR review feedback. Do NOT commit.

Working directory: <WORKTREE_PATH>
Branch: <BRANCH>

The following issues were identified in a PR review and must be fixed:

<FOR EACH FINDING:>
[<SEVERITY>] <FILE:LINE>
Issue: <description>
Suggested fix: <suggestion>
---

Fix all MUST FIX items. Fix SHOULD FIX items unless there is a compelling reason not to.
For each CONSIDER item, use your judgment — fix it only if it's trivially obvious.

After fixing:
- Run `make test` (from <WORKTREE_PATH>)
- Run `make lint` (from <WORKTREE_PATH>)
- If frontend changed: `cd frontend && npm run typecheck && npm run lint`

Handoff report format:
## HANDOFF REPORT
### Findings Addressed
- [MUST FIX] <file:line>: <what you did>
### Findings Skipped
- [SHOULD FIX] <file:line>: <reason skipped>
### Verification
- Tests: PASS/FAIL
- Lint: PASS/FAIL
### Outstanding Issues
<anything still broken or needing coordinator attention>

Do NOT commit, push, or run git commands.
```

After the fix sub-agent completes:
1. Commit the review fixes (Phase 5 process with type `refactor` or `fix`)
2. Push
3. Re-run CI monitoring (Phase 7) — ensure fixes didn't break anything
4. If CI passes, proceed to Phase 9

---

## Phase 9 — Promote PR to Ready

Convert the PR from draft to ready for review:

```bash
gh pr ready <PR_NUMBER>
```

Then post a summary comment on the original issue:

```bash
gh issue comment <NUMBER> --body "$(cat <<'EOF'
Implementation complete. PR #<PR_NUMBER> is ready for review.

**Branch:** \`<BRANCH>\`
**PR:** <PR_URL>

Summary of changes:
<3-4 bullet points from the handoff report>
EOF
)"
```

---

## Phase 10 — Final Report

Output a summary to the user:

```
## implement-issue Workflow Complete

**Issue:** #<NUMBER> — <TITLE>
**PR:** <PR_URL> (ready for review)
**Branch:** <BRANCH>
**Worktree:** <WORKTREE_PATH>

### What was done
- Issue validated and claimed (#<NUMBER> assigned to you, labeled in-progress)
- Worktree created at <WORKTREE_PATH>
- Implementation delegated to engineering sub-agent
- <N> commits pushed to <BRANCH>
- CI: all <N> checks passing
- PR review: <N> findings fixed
- PR promoted from draft to ready

### Changes
<List files changed from handoff report>

### Next steps
- Review and merge: <PR_URL>
- To clean up the worktree after merge: `git worktree remove <WORKTREE_PATH>`
```

---

## Coordinator Rules

- **YOU own git.** Sub-agents implement code; you stage, commit, push.
- **Never `--no-verify`.** If hooks fail, fix the underlying code.
- **Never `--force` push** to main. Force-push to feature branches is allowed only if you made an error in the last commit and haven't pushed yet.
- **Always NEW commits.** Never `--amend` unless you haven't pushed yet.
- **Be specific with `git add`.** Never `git add .` blindly — check `git status` first.
- **Sub-agents get full context.** Never call a sub-agent with vague instructions — paste the issue, the handoff report, the failure logs.
- **Sub-agents stop after their deliverable.** They do not loop or wait — they run, report, and stop.
- **Validate before promoting.** Never promote draft to ready until CI passes and review findings are addressed.
- **Respect the user.** If you hit a situation where you cannot proceed autonomously (ambiguous requirements, persistent failures, design decisions), pause and ask clearly.
