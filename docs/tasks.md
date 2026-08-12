# Scheduled tasks

A task runs an agent on a schedule and records every execution. Create and
manage them under **Tasks** in the UI.

- [Creating a task](#creating-a-task)
- [Schedule types](#schedule-types)
- [Stop conditions](#stop-conditions)
- [Job history](#job-history)
- [Notifications](#notifications)
- [API](#api)

---

## Creating a task

| Field | Required | Notes |
|-------|----------|-------|
| Name | Yes | Shown in the task list and in notifications |
| Description | No | Free text |
| Prompt | Yes | What the agent is asked to do on every run |
| Agent | No | Which saved [agent](agents.md) runs it. Without one, the default model and no system prompt are used |
| Working directory | No | Where the run executes. Defaults to `AGENTO_WORKING_DIR` |
| Model | No | Overrides the agent's model for this task |
| Settings profile | No | Which [Claude settings profile](agents.md#claude-settings-profiles) the run uses |
| Timeout | No | Minutes before the run is abandoned |
| Save output | No | Keep the full response text in job history |

The prompt supports the same `{{current_date}}` and `{{current_time}}`
placeholders as an agent's system prompt.

A task runs unattended, so the agent's [permission mode](security.md#agent-permission-modes)
matters more than usual — nobody is there to answer a prompt.

---

## Schedule types

| Type | Configuration | Behaviour |
|------|---------------|-----------|
| `run_immediately` | — | Runs once, as soon as it is saved. The default when no schedule is chosen |
| `one_off` | A date and time | Runs once, then goes idle |
| `interval` | Every N minutes / hours / days, optionally at a fixed `HH:MM` for daily intervals | Repeats |
| `cron` | A cron expression | Repeats |

The task list shows each task's last run, its outcome, and when it is next due.
A task can be **paused** and **resumed** without losing its history; resuming
recalculates the next run.

---

## Stop conditions

A repeating task can be told to stop by itself:

- **after N runs** — `stop_after_count`
- **after a date** — `stop_after_time`

Both are optional; without them the task repeats indefinitely until paused or
deleted.

---

## Job history

Every execution is recorded, whether it succeeded or not:

- status (`running`, `success`, `failed`), start time and duration
- the model used and the chat session the run created
- input, output, cache-read and cache-write token counts
- the error message on failure, and the full response text when **Save output**
  is on

Browse it per task from the task's page, or across all tasks under **Job
History**, where old records can be deleted in bulk.

---

## Notifications

With SMTP configured under **Settings → Notifications**, Agento can email you
when a task finishes and when one fails — each toggled separately, both on by
default. Send a test message from the same tab to verify the configuration, and
check the notification log to see what was delivered.

---

## API

| Endpoint | Purpose |
|----------|---------|
| `GET/POST /api/tasks` | List and create |
| `GET/PUT/DELETE /api/tasks/{id}` | Read, update, delete |
| `POST /api/tasks/{id}/pause` · `/resume` | Pause and resume |
| `GET /api/tasks/{id}/job-history` | One task's runs |
| `GET/DELETE /api/job-history` | All runs; bulk delete |
| `GET/DELETE /api/job-history/{id}` | One run |
| `GET/PUT /api/notifications/settings` | Notification configuration |
| `POST /api/notifications/test` | Send a test email |
| `GET /api/notifications/log` | Delivery log |

State-changing requests must send `Content-Type: application/json` — see
[Security](security.md#browser-based-protections).
