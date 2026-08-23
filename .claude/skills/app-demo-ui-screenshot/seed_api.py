#!/usr/bin/env python3
"""Seed agents, integrations and scheduled tasks into the isolated dev instance
through its own API (fake credentials; format-valid only, never dialled)."""
import json, os, sys, urllib.request

FAKEHOME = sys.argv[1]
BASE = "http://127.0.0.1:8991/api"
TOKEN = open(f"{FAKEHOME}/.agento-desktop-dev/api-token").read().strip()


def call(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method,
                                 headers={"Authorization": "Bearer " + TOKEN,
                                          "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req) as r:
            raw = r.read()
            return r.status, (json.loads(raw) if raw.strip() else None)
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()


# ---- integrations --------------------------------------------------------
integrations = [
    {"name": "GitHub (acme)", "type": "github", "enabled": True,
     "credentials": {"auth_mode": "pat", "personal_access_token": "ghp_" + "Q" * 36},
     "services": {"repos": {"enabled": True, "tools": []}, "issues": {"enabled": True, "tools": []},
                  "pull_requests": {"enabled": True, "tools": []}, "actions": {"enabled": True, "tools": []},
                  "releases": {"enabled": False, "tools": []}}},
    {"name": "Engineering Slack", "type": "slack", "enabled": True,
     "credentials": {"auth_mode": "bot_token", "bot_token": "xoxb-0000000000-0000000000000-" + "x" * 24},
     "services": {"messaging": {"enabled": True, "tools": []}}},
    {"name": "Acme Jira", "type": "jira", "enabled": True,
     "credentials": {"site_url": "https://acme.atlassian.net", "email": "platform@acme.example",
                     "api_token": "ATATT3" + "x" * 40},
     "services": {"project_management": {"enabled": True, "tools": []}}},
    {"name": "On-call Telegram bot", "type": "telegram", "enabled": True,
     "credentials": {"bot_token": "123456789:AAF" + "x" * 32},
     "services": {"messaging": {"enabled": True, "tools": []}}},
]
ids = {}
for it in integrations:
    status, res = call("POST", "/integrations", it)
    print("integration", it["type"], status, (res.get("id") if isinstance(res, dict) else res))
    if isinstance(res, dict):
        ids[it["type"]] = res["id"]

gh = ids.get("github")
slack = ids.get("slack")
jira = ids.get("jira")

# ---- agents --------------------------------------------------------------
agents = [
    {"name": "Code Reviewer", "slug": "code-reviewer",
     "description": "Reviews a diff for correctness, security and style",
     "model": "claude-opus-5", "thinking": "adaptive", "permission_mode": "plan",
     "system_prompt": "You are a meticulous code reviewer. Today is {{current_date}}.\n\nReview the change for correctness first, then security, then style. Report findings ordered by severity with file and line. Do not modify files; propose patches inline. Stay silent on nits unless asked.",
     "capabilities": {"built_in": ["Read", "Grep", "Glob", "Bash"], "local": ["current_time"],
                      "mcp": ({gh: {"tools": ["get_pull_request", "get_pull_diff", "list_pull_requests", "get_issue"]}} if gh else None)},
     "claude_config_dir": ""},
    {"name": "Daily Standup Bot", "slug": "standup-bot",
     "description": "Posts a morning summary of yesterday's commits and open PRs",
     "model": "claude-sonnet-5", "thinking": "disabled", "permission_mode": "bypass",
     "system_prompt": "Every weekday morning, summarise what changed since the previous working day: merged PRs, open PRs waiting on review, and any failing workflow. Post it to #eng-standup in under 200 words. Date: {{current_date}}.",
     "capabilities": {"built_in": ["Bash"], "local": ["current_time"],
                      "mcp": {k: v for k, v in {
                          gh: {"tools": ["list_pull_requests", "list_workflow_runs"]},
                          slack: {"tools": ["send_message"]}}.items() if k}},
     "claude_config_dir": ""},
    {"name": "Release Notes Writer", "slug": "release-notes",
     "description": "Summarises merged pull requests into release notes",
     "model": "claude-sonnet-5", "thinking": "adaptive", "permission_mode": "default",
     "system_prompt": "Turn the pull requests merged since the last tag into release notes grouped by area (Added, Changed, Fixed). One line per change, link the PR, no internal ticket ids. Write to CHANGELOG.md under a new heading for the version you are given.",
     "capabilities": {"built_in": ["Read", "Edit", "Write", "Bash", "Grep"], "local": [],
                      "mcp": ({gh: {"tools": ["list_pull_requests", "get_pull_request", "list_releases"]}} if gh else None)},
     "claude_config_dir": ""},
    {"name": "Repo Librarian", "slug": "repo-librarian",
     "description": "Answers questions about the codebase without changing it",
     "model": "claude-haiku-4-5-20251001", "thinking": "disabled", "permission_mode": "default",
     "system_prompt": "Answer questions about this repository by reading it. Cite file paths and line numbers. Never edit, write or run anything that changes state.",
     "capabilities": {"built_in": ["Read", "Grep", "Glob"], "local": [], "mcp": None},
     "claude_config_dir": ""},
    {"name": "Incident Scribe", "slug": "incident-scribe",
     "description": "Drafts the incident timeline from Jira, Slack and git history",
     "model": "claude-opus-5", "thinking": "enabled", "permission_mode": "plan",
     "system_prompt": "Given an incident id, reconstruct the timeline from the Jira ticket, the Slack incident channel and the git history of the affected services. Produce docs/incidents/<date>.md following the template. Flag anything you could not verify.",
     "capabilities": {"built_in": ["Read", "Write", "Bash", "Grep"], "local": ["current_time"],
                      "mcp": {k: v for k, v in {
                          jira: {"tools": ["get_issue", "search_issues"]},
                          slack: {"tools": ["read_messages", "search_messages"]}}.items() if k}},
     "claude_config_dir": ""},
]
for a in agents:
    status, res = call("POST", "/agents", a)
    print("agent", a["slug"], status, res if status >= 300 else "")

# ---- tasks ---------------------------------------------------------------
tasks = [
    {"name": "Draft release notes", "description": "Turn merged pull requests into draft release notes",
     "prompt": "Draft release notes for everything merged since the last tag and open them as a draft PR.",
     "agent_slug": "release-notes", "working_directory": os.path.expanduser("~") + "/Projects/platform", "model": "",
     "settings_profile_id": "", "timeout_minutes": 30, "schedule_type": "interval",
     "schedule_config": {"every_days": 1, "at_time": "18:00"}, "stop_after_count": 0, "stop_after_time": None,
     "save_output": True, "status": "active"},
    {"name": "Weekly dependency review", "description": "Check for outdated and vulnerable dependencies",
     "prompt": "Run the dependency audit across the service repos and open one issue per actionable finding.",
     "agent_slug": "code-reviewer", "working_directory": os.path.expanduser("~") + "/Projects/gateway", "model": "",
     "settings_profile_id": "", "timeout_minutes": 45, "schedule_type": "cron",
     "schedule_config": {"expression": "0 9 * * 1"}, "stop_after_count": 0, "stop_after_time": None,
     "save_output": True, "status": "active"},
    {"name": "Morning standup summary", "description": "Summarise yesterday's commits and open pull requests",
     "prompt": "Post the standup summary for today.",
     "agent_slug": "standup-bot", "working_directory": os.path.expanduser("~") + "/Projects/platform", "model": "",
     "settings_profile_id": "", "timeout_minutes": 15, "schedule_type": "cron",
     "schedule_config": {"expression": "0 8 * * 1-5"}, "stop_after_count": 0, "stop_after_time": None,
     "save_output": True, "status": "active"},
    {"name": "Nightly flaky-test triage", "description": "Re-run the last failed CI job and file an issue if it is flaky",
     "prompt": "Find last night's failed workflow runs, re-run the failing job once, and file an issue for anything that passes on retry.",
     "agent_slug": "code-reviewer", "working_directory": os.path.expanduser("~") + "/Projects/payments", "model": "claude-sonnet-5",
     "settings_profile_id": "", "timeout_minutes": 60, "schedule_type": "cron",
     "schedule_config": {"expression": "30 2 * * *"}, "stop_after_count": 0, "stop_after_time": None,
     "save_output": True, "status": "paused"},
]
for t in tasks:
    status, res = call("POST", "/tasks", t)
    print("task", t["name"], status, res if status >= 300 else "")
print(json.dumps(ids))
