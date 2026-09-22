import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "../lib/api";
import type {
  Agent,
  JobHistory,
  ScheduleConfig,
  ScheduleType,
  ScheduledTask,
  SettingsResponse,
  TaskPreview,
  TaskRunStarted,
} from "../lib/types";
import {
  describeError,
  fieldOf,
  useDebounced,
  usePoll,
  useResource,
} from "../lib/hooks";
import {
  loadTaskFormPrefs,
  saveTaskFormPrefs,
  type TaskFormSectionId,
} from "../lib/taskFormPrefs";
import { DESTROY, partnerLabel, submitLabel } from "../lib/formVerbs";
import { dateTime, duration, relativeTime, toneFor } from "../lib/format";
import { Icon } from "../lib/icons";
import type { NavigateFn } from "../lib/nav";
import { CopyButton } from "../components/CopyButton";
import { DirField, useDirPicker } from "../components/DirField";
import {
  Dropdown,
  Empty,
  FormRow,
  FormSection,
  InspGroup,
  InspRow,
  Search,
  Segmented,
  Splitter,
  Switch,
} from "../components/ui";
import "../styles/tasks.css";

const POLL_MS = 10_000;

/* --- Schedule model ------------------------------------------------------- */

const SCHEDULE_OPTIONS: { value: ScheduleType; label: string }[] = [
  { value: "run_immediately", label: "Immediately" },
  { value: "one_off", label: "Once" },
  { value: "interval", label: "Interval" },
  { value: "cron", label: "Cron" },
];

type IntervalUnit = "minutes" | "hours" | "days";

function intervalUnit(c: ScheduleConfig): IntervalUnit {
  if (c.every_days) return "days";
  if (c.every_hours) return "hours";
  return "minutes";
}

function intervalCount(c: ScheduleConfig): number {
  return c.every_days ?? c.every_hours ?? c.every_minutes ?? 1;
}

/**
 * Build the config from scratch on every edit: the server reads whichever
 * `every_*` key is present, so a leftover key from a previous unit would
 * silently win over the one the user just set.
 */
function intervalConfig(
  unit: IntervalUnit,
  count: number,
  atTime: string
): ScheduleConfig {
  const n = Math.max(1, Math.round(count) || 1);
  if (unit === "days") return atTime ? { every_days: n, at_time: atTime } : { every_days: n };
  if (unit === "hours") return { every_hours: n };
  return { every_minutes: n };
}

/** A valid config for a type the user just switched to. */
function configForType(type: ScheduleType, prev: ScheduleConfig): ScheduleConfig {
  switch (type) {
    case "run_immediately":
      return {};
    case "one_off":
      return { run_at: prev.run_at ?? new Date(Date.now() + 3_600_000).toISOString() };
    case "interval":
      return intervalConfig(intervalUnit(prev), intervalCount(prev), prev.at_time ?? "");
    case "cron":
      return { expression: prev.expression ?? "0 9 * * *" };
  }
}

function describeSchedule(t: ScheduledTask): string {
  const c = t.schedule_config ?? {};
  switch (t.schedule_type) {
    case "run_immediately":
      return "Immediately";
    case "one_off":
      return `Once · ${dateTime(c.run_at)}`;
    case "cron":
      return `Cron · ${c.expression || "—"}`;
    case "interval": {
      const n = intervalCount(c);
      const unit = intervalUnit(c);
      const noun = n === 1 ? unit.slice(0, -1) : `${n} ${unit}`;
      const at = unit === "days" && c.at_time ? ` at ${c.at_time}` : "";
      return `Every ${noun}${at}`;
    }
    default:
      return "—";
  }
}

/* --- Collapsed-section summaries (#631) ----------------------------------- */

/** What the server stores for a `timeout_minutes` of 0 (`DEFAULT_TIMEOUT_MINUTES`
 *  in `native/tasks.rs`), so the summary says what the run will actually get. */
const DEFAULT_TIMEOUT_MINUTES = 30;

/** Execution, collapsed: directory (when set) · model · timeout · output. */
function executionSummary(t: ScheduledTask): string {
  return [
    t.working_directory,
    t.model || "default model",
    `${t.timeout_minutes || DEFAULT_TIMEOUT_MINUTES} min`,
    t.save_output ? "output saved" : "output not saved",
  ]
    .filter(Boolean)
    .join(" · ");
}

/** Limits, collapsed: `no limit`, or whichever of the two limits is set. */
function limitsSummary(t: ScheduledTask): string {
  const parts = [
    t.stop_after_count > 0 ? `stops after ${t.stop_after_count} run${t.stop_after_count === 1 ? "" : "s"}` : "",
    t.stop_after_time ? `ends ${dateTime(t.stop_after_time)}` : "",
  ].filter(Boolean);
  return parts.length ? parts.join(" · ") : "no limit";
}

/** The fields each collapsible section owns — a 422 naming one opens it. */
const SECTION_FIELDS: Record<TaskFormSectionId, readonly string[]> = {
  execution: ["working_directory", "model", "timeout_minutes", "save_output"],
  limits: ["stop_after_count", "stop_after_time"],
};

/* --- Local <-> RFC3339 for the native pickers ----------------------------- */

function toLocalStamp(iso: string | null | undefined): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (!isFinite(d.getTime())) return "";
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(
    d.getHours()
  )}:${p(d.getMinutes())}`;
}

function fromLocalStamp(v: string): string {
  const d = new Date(v);
  return isFinite(d.getTime()) ? d.toISOString() : "";
}

/* --- The two "no limit" defaults ------------------------------------------
   Both limits are stored as their *unset* value — `stop_after_count: 0` and
   `stop_after_time: null` — and `0` is the longer horizon, not the shorter
   one, the same way round as the gateway's `usage_retention_days`. The form
   therefore never shows either unset value: switching a limit *on* has to
   seed something a user would plausibly have typed, or the control lands on
   the number it exists to stop meaning. -------------------------------------- */

/** How many *further* runs "Run limit" starts at.
 *
 *  Ten more, not a flat ten: `should_auto_pause` is
 *  `stop_after_count > 0 && run_count >= stop_after_count`, evaluated *before*
 *  the run and answered by declining it **silently** — no `job_history` row,
 *  nothing on screen. So seeding a bare `10` on a task with 42 runs behind it
 *  would hand the user a value that stops their schedule for good the moment
 *  they save, with no record saying why. */
const FIRST_STOP_AFTER_COUNT = 10;

/** What "Run limit" starts at for a task that has already run this often. */
function firstStopAfterCount(runCount: number): number {
  return FIRST_STOP_AFTER_COUNT + Math.max(0, runCount);
}

/** What each limit was last set to, so flicking a mode switch off and back on
 *  restores it instead of re-seeding.
 *
 *  This is a **memory, never a source of truth** — the mode stays derived from
 *  the stored value, so there is no second copy of it to fall out of step with
 *  the draft. It carries the task id it belongs to, so a value stashed on one
 *  task cannot surface on another. */
type LimitStash = { id: string; count: number; time: string | null };

/** The stash, but only when it belongs to this task. */
function stashFor(s: LimitStash, id: string): LimitStash {
  return s.id === id ? s : { id, count: 0, time: null };
}

/** What "End date" starts at: a week out, truncated to the minute the picker
 *  shows — so the stored instant and the displayed one agree from the start
 *  rather than only after the first edit. */
function firstStopAfterTime(): string {
  const d = new Date(Date.now() + 7 * 86_400_000);
  d.setSeconds(0, 0);
  return d.toISOString();
}

/* --- New-task template ---------------------------------------------------- */

function blankTask(): ScheduledTask {
  const now = new Date().toISOString();
  return {
    id: "",
    name: "New task",
    description: "",
    prompt: "",
    agent_slug: "",
    working_directory: "",
    model: "",
    settings_profile_id: "",
    timeout_minutes: DEFAULT_TIMEOUT_MINUTES,
    schedule_type: "interval",
    schedule_config: { every_hours: 24 },
    stop_after_count: 0,
    stop_after_time: null,
    save_output: true,
    status: "active",
    run_count: 0,
    last_run_at: null,
    last_run_status: "",
    next_run_at: null,
    created_at: now,
    updated_at: now,
  };
}

/** The Model field's placeholder: the model the run will use when the field
 *  is left empty. With no agent that is the Settings default model — the same
 *  `settings::resolve` the executor's `TurnSettings::default_model` reads; with
 *  an agent it is the agent's own model, which `run_agent` always applies. */
function modelPlaceholder(slug: string, agents: Agent[], defaultModel: string): string {
  if (!slug) return defaultModel ? `Default model (${defaultModel})` : "Default model";
  const agent = agents.find((a) => a.slug === slug);
  return agent?.model ? `Agent's model (${agent.model})` : "Agent default";
}

/* --- View ----------------------------------------------------------------- */

export function TasksView({
  inspectorOpen,
  onNavigate,
}: {
  inspectorOpen: boolean;
  /**
   * The cross-view hand-off, as a prop rather than `useNavigate` (#542).
   *
   * `App` renders this view directly, so there is nothing to thread a callback
   * through — which is the whole of `nav.ts`'s prop-versus-context rule, and
   * the same reason the three gateway views take one.
   */
  onNavigate: NavigateFn;
}) {
  const picker = useDirPicker();
  const tasksRes = useResource<ScheduledTask[] | null>(
    (signal) => api.get("/tasks", signal),
    []
  );
  const agentsRes = useResource<Agent[] | null>(
    (signal) => api.get("/agents", signal),
    []
  );
  const settingsRes = useResource<SettingsResponse | null>(
    (signal) => api.get<SettingsResponse>("/settings", signal),
    []
  );
  const defaultModel = settingsRes.data?.settings.default_model ?? "";

  const tasks = useMemo(() => tasksRes.data ?? [], [tasksRes.data]);
  const agents = useMemo(() => agentsRes.data ?? [], [agentsRes.data]);

  usePoll(tasksRes.reload, POLL_MS);

  const [query, setQuery] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [draft, setDraft] = useState<ScheduledTask | null>(null);
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  /**
   * Starting a manual run, kept **separate from `busy`** (#541).
   *
   * `busy` is already shared by save, pause/resume and delete, which is why the
   * submit button below cannot use it as an in-flight label. Folding a third
   * verb into it would widen that problem; a run also has to disable a
   * different button from the one a save disables.
   */
  const [starting, setStarting] = useState(false);
  /**
   * The `job_history` id the last **Run now** answered with, held until that
   * row reaches a terminal status (#541).
   *
   * `starting` covers the *request*, which returns in milliseconds; the run
   * itself waits for one of the scheduler's three permits and can then take
   * hours, so clearing on the response would put the button back to "Run now"
   * while the run it started had not begun. The row may not exist yet either,
   * which is why "unknown id" reads as in flight rather than as finished.
   */
  const [startedJobId, setStartedJobId] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string>();
  /** Which collapsible sections are open, as the user left them. */
  const [sections, setSections] = useState(loadTaskFormPrefs);
  /** Set when the user closes a section an error forced open; any new error resets it. */
  const [autoOpenDismissed, setAutoOpenDismissed] = useState(false);
  useEffect(() => setAutoOpenDismissed(false), [actionError]);
  const [confirmDelete, setConfirmDelete] = useState(false);
  /** See `LimitStash` — what the two Limits switches restore when flicked back on. */
  const lastLimits = useRef<LimitStash>({ id: "", count: 0, time: null });

  const selected = selectedId ? tasks.find((t) => t.id === selectedId) ?? null : null;

  // Select something as soon as there is something to select.
  useEffect(() => {
    if (creating || tasks.length === 0) return;
    if (!selectedId || !tasks.some((t) => t.id === selectedId)) {
      setSelectedId(tasks[0].id);
    }
  }, [tasks, selectedId, creating]);

  // Re-seed the form from the server, but never on top of unsaved edits — the
  // 10s poll would otherwise wipe out whatever is being typed.
  const stamp = selected ? `${selected.id}@${selected.updated_at}` : "";
  useEffect(() => {
    if (creating) return;
    if (!selected) {
      setDraft(null);
      setDirty(false);
      return;
    }
    if (dirty && draft && draft.id === selected.id) return;
    setDraft(selected);
    setDirty(false);
    setConfirmDelete(false);
    setActionError(undefined);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [stamp, creating]);

  const historyRes = useResource<JobHistory[] | null>(
    (signal) =>
      selectedId && !creating
        ? api.get(`/tasks/${selectedId}/job-history`, signal)
        : Promise.resolve(null),
    [selectedId, creating]
  );
  usePoll(historyRes.reload, POLL_MS, !!selectedId && !creating);

  // The create-mode inspector's preview (#633). The server computes it with
  // the scheduler's own code, so nothing here parses a cron or does schedule
  // arithmetic; this only debounces the draft and aborts a stale request.
  const previewInput = useDebounced(creating ? draft : null, 250);
  const previewKey = JSON.stringify(previewInput);
  const previewRes = useResource<TaskPreview | null>(
    (signal) =>
      previewInput && inspectorOpen
        ? api.post<TaskPreview>("/tasks/preview", previewInput, signal)
        : Promise.resolve(null),
    [previewKey, inspectorOpen]
  );

  const startedJob = useMemo(
    () =>
      startedJobId
        ? (historyRes.data ?? []).find((j) => j.id === startedJobId)
        : undefined,
    [historyRes.data, startedJobId]
  );
  /** The started run has landed and finished, so the button is free again. */
  useEffect(() => {
    if (startedJob && startedJob.status !== "running") setStartedJobId(null);
  }, [startedJob]);
  /** Selecting another task drops the previous one's pending run. */
  useEffect(() => {
    setStartedJobId(null);
  }, [selectedId]);

  /** The request, or the run it started — either shuts the action strip. */
  const runInFlight = starting || startedJobId !== null;

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return tasks;
    return tasks.filter((t) =>
      [t.name, t.description, t.agent_slug, t.prompt]
        .join(" ")
        .toLowerCase()
        .includes(q)
    );
  }, [tasks, query]);

  const enabled = filtered.filter((t) => t.status === "active");
  const paused = filtered.filter((t) => t.status !== "active");

  /** The section the current error forces open, if any — until it is closed by hand. */
  function forcedOpen(id: TaskFormSectionId): boolean {
    const field = fieldOf(actionError);
    return !autoOpenDismissed && !!field && SECTION_FIELDS[id].includes(field);
  }

  /** Open as stored, or forced open while the last error names a field in it.
   *  Derived rather than written back, so a 422 never changes the preference. */
  function isOpen(id: TaskFormSectionId): boolean {
    return sections[id] || forcedOpen(id);
  }

  function toggleSection(id: TaskFormSectionId) {
    const open = !isOpen(id);
    // Closing a section the error forced open has to stick, or the click would
    // do nothing visible while the error is still on screen.
    if (!open && forcedOpen(id)) setAutoOpenDismissed(true);
    const next = { ...sections, [id]: open };
    setSections(next);
    saveTaskFormPrefs(next);
  }

  function edit(patch: Partial<ScheduledTask>) {
    setDraft((d) => (d ? { ...d, ...patch } : d));
    setDirty(true);
  }

  function startCreate() {
    setCreating(true);
    setConfirmDelete(false);
    setActionError(undefined);
    setDraft(blankTask());
    lastLimits.current = { id: "", count: 0, time: null };
    setDirty(true);
  }

  function select(id: string) {
    setCreating(false);
    setSelectedId(id);
  }

  async function save() {
    if (!draft) return;
    setBusy(true);
    setActionError(undefined);
    try {
      if (creating) {
        const created = await api.post<ScheduledTask | undefined>("/tasks", draft);
        setCreating(false);
        setDirty(false);
        if (created?.id) {
          setDraft(created);
          setSelectedId(created.id);
        }
      } else {
        const updated = await api.put<ScheduledTask | undefined>(
          `/tasks/${draft.id}`,
          draft
        );
        setDirty(false);
        if (updated?.id) setDraft(updated);
      }
      tasksRes.reload();
    } catch (err) {
      setActionError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  function revert() {
    if (creating) {
      setCreating(false);
      setDraft(selected);
    } else {
      setDraft(selected);
    }
    // The stash remembers edits; a revert throws those edits away.
    lastLimits.current = { id: "", count: 0, time: null };
    setDirty(false);
    setActionError(undefined);
  }

  /**
   * `POST /api/tasks/{id}/run` (#541) — the verb the ▶ glyph used to be
   * mistaken for, now its own labelled control beside the one that means
   * *enabled*.
   *
   * The route answers `202` as soon as the run is *accepted*, which is not the
   * same as started — so the id it answers with is kept, and the strip stays
   * shut until that row turns up in the history below with a terminal status.
   * `historyRes` is already polling, so nothing new has to be scheduled here.
   *
   * It deliberately does not reload `tasksRes` — a manual run changes nothing
   * about the task row, which is the whole point of it.
   */
  async function runNow() {
    if (!draft?.id) return;
    setStarting(true);
    setActionError(undefined);
    try {
      const started = await api.post<TaskRunStarted>(`/tasks/${draft.id}/run`);
      setStartedJobId(started?.job_id ?? null);
      historyRes.reload();
    } catch (err) {
      setActionError(describeError(err));
    } finally {
      setStarting(false);
    }
  }

  async function toggleStatus() {
    if (!draft?.id) return;
    const next: "paused" | "active" = draft.status === "active" ? "paused" : "active";
    setBusy(true);
    setActionError(undefined);
    try {
      await api.post(`/tasks/${draft.id}/${next === "paused" ? "pause" : "resume"}`);
      // Reflect it immediately: a dirty draft is not re-seeded by the reload.
      setDraft((d) => (d ? { ...d, status: next } : d));
      tasksRes.reload();
    } catch (err) {
      setActionError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    if (!draft?.id) return;
    setBusy(true);
    setActionError(undefined);
    try {
      await api.del(`/tasks/${draft.id}`);
      setConfirmDelete(false);
      setSelectedId(null);
      setDraft(null);
      setDirty(false);
      tasksRes.reload();
    } catch (err) {
      setActionError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  const listLoading = tasksRes.loading && !tasksRes.data;
  const listError = tasksRes.error && !tasksRes.data;

  return (
    <div className="panes">
      {picker.browser}
      <div className="pane-list">
        <div className="listhead">
          <div className="listhead__row">
            <Search value={query} onChange={setQuery} placeholder="Search tasks" />
            <button className="iconbtn" title="New task" onClick={startCreate}>
              <Icon name="plus" size={14} />
            </button>
          </div>
        </div>

        {listLoading ? (
          <div className="statepane">Loading tasks…</div>
        ) : listError ? (
          <Empty
            icon="alert"
            title="Couldn't load tasks"
            text={tasksRes.error ?? ""}
            action={
              <button className="btn" onClick={tasksRes.reload}>
                <Icon name="refresh" size={13} />
                Retry
              </button>
            }
          />
        ) : tasks.length === 0 ? (
          <Empty
            icon="task"
            title="No scheduled tasks"
            text="A task runs an agent on a schedule and keeps every run in its history."
            action={
              <button className="btn btn--primary" onClick={startCreate}>
                <Icon name="plus" size={13} />
                New task
              </button>
            }
          />
        ) : filtered.length === 0 ? (
          <Empty icon="search" title="No matches" text={`Nothing matches “${query}”.`} />
        ) : (
          <div className="list__scroll scroll">
            {enabled.length > 0 && <div className="listgroup">Enabled</div>}
            {enabled.map((t) => (
              <TaskRow
                key={t.id}
                t={t}
                active={!creating && t.id === selectedId}
                onSelect={select}
              />
            ))}
            {paused.length > 0 && <div className="listgroup">Paused</div>}
            {paused.map((t) => (
              <TaskRow
                key={t.id}
                t={t}
                active={!creating && t.id === selectedId}
                onSelect={select}
              />
            ))}
          </div>
        )}
      </div>

      <Splitter variable="--list-w" min={240} max={460} />

      <div className="pane-detail">
        {!draft ? (
          <Empty
            icon="task"
            title="No task selected"
            text="Pick a task on the left to edit its schedule and instruction."
          />
        ) : (
          <>
            <div className="toolbar">
              <div className="toolbar__title">
                {creating ? "New task" : draft.name || "Untitled task"}
              </div>
              {!creating && <StatusBadge status={draft.last_run_status} />}
              <div className="spacer" />

              {confirmDelete ? (
                <div className="confirm">
                  <span className="confirm__text">
                    {DESTROY} {draft.name || "Untitled task"}? Its run history goes with it.
                  </span>
                  <button
                    className="btn btn--ghost"
                    onClick={() => setConfirmDelete(false)}
                  >
                    Cancel
                  </button>
                  <button className="btn btn--danger" onClick={remove} disabled={busy}>
                    {DESTROY}
                  </button>
                </div>
              ) : (
                <>
                  {(dirty || creating) && (
                    <button className="btn btn--ghost" onClick={revert} disabled={busy}>
                      {partnerLabel(creating)}
                    </button>
                  )}
                  <button
                    className="btn btn--primary"
                    onClick={save}
                    disabled={busy || runInFlight || (!dirty && !creating)}
                  >
                    {/* Never the in-flight label: this view's `busy` is shared
                        with pause/resume and delete, so passing it here would
                        make the submit read "Saving…" while an unrelated
                        request runs. The disabled state already covers that
                        case, and `runInFlight` joins it for the same
                        reason. */}
                    {submitLabel(creating, false)}
                  </button>
                  {!creating && (
                    <>
                      <div className="toolbar__sep" />
                      {/* Two verbs, two words. This strip used to carry one
                          ▶/⏸ icon button whose only affordance was a `title`,
                          and ▶ is the universal *execute* glyph — so the one
                          control that meant "enabled" was drawn as the one that
                          means "run", and the run it was mistaken for did not
                          exist anywhere in the product (#541). Neither carries
                          a `+`: both act on a record that already exists, which
                          is the rule in CLAUDE.md → *A form's actions are a
                          fixed grammar*. */}
                      {/* Disabled while the draft is dirty — which is exactly
                          when `save` above is enabled. The run reads the stored
                          *row*; the route never sees the draft, so running with
                          unsaved edits would test the previous configuration
                          and file a job_history row that reads as a test of
                          what is on screen. */}
                      <button
                        className="btn"
                        onClick={runNow}
                        disabled={busy || runInFlight || dirty}
                      >
                        {starting
                          ? "Starting…"
                          : startedJobId
                            ? "Running…"
                            : "Run now"}
                      </button>
                      <button
                        className="btn"
                        onClick={toggleStatus}
                        disabled={busy || runInFlight}
                      >
                        {draft.status === "active" ? "Disable" : "Enable"}
                      </button>
                      <button
                        className="iconbtn"
                        title={DESTROY}
                        onClick={() => setConfirmDelete(true)}
                      >
                        <Icon name="trash" size={14} />
                      </button>
                    </>
                  )}
                </>
              )}
            </div>

            <div className="scroll tasks-form" style={{ flex: 1, padding: "var(--sp-8)" }}>
              <div className="form">
                {actionError && <div className="formerror">{actionError}</div>}

                <div className="formsec">
                  <div className="formsec__title">Task</div>
                  <FormRow label="Name">
                    <label className="field">
                      <input
                        value={draft.name}
                        onChange={(e) => edit({ name: e.target.value })}
                        spellCheck={false}
                      />
                    </label>
                  </FormRow>
                  <FormRow label="Description">
                    <label className="field">
                      <input
                        value={draft.description}
                        onChange={(e) => edit({ description: e.target.value })}
                        placeholder="Optional"
                      />
                    </label>
                  </FormRow>
                  <FormRow
                    label="Agent"
                    help={
                      draft.agent_slug
                        ? undefined
                        : "Runs Claude Code with the default model (or the one set below) and all built-in tools, no system prompt and no integrations. Permission prompts are skipped." +
                          (agentsRes.data && agents.length === 0
                            ? " Create agents in Agents to give a task a system prompt and integrations."
                            : "")
                    }
                  >
                    <AgentPicker
                      value={draft.agent_slug}
                      agents={agents}
                      onChange={(slug) => edit({ agent_slug: slug })}
                    />
                  </FormRow>
                  {!creating && (
                    <FormRow
                      label="Enabled"
                      help="Paused tasks keep their history but never fire."
                    >
                      {/* The same guard as the toolbar's twin above: both
                          drive `toggleStatus`, so one of them staying live
                          during a write would be the asymmetry, not a mercy. */}
                      <Switch
                        on={draft.status === "active"}
                        onChange={toggleStatus}
                        disabled={busy || runInFlight}
                      />
                    </FormRow>
                  )}
                </div>

                <div className="divider" />

                <div className="formsec">
                  <div className="formsec__title">Schedule</div>
                  <ScheduleEditor
                    type={draft.schedule_type}
                    config={draft.schedule_config ?? {}}
                    onChange={(schedule_type, schedule_config) =>
                      edit({ schedule_type, schedule_config })
                    }
                  />
                </div>

                <div className="divider" />

                <FormSection
                  title="Execution"
                  summary={executionSummary(draft)}
                  collapsible
                  open={isOpen("execution")}
                  onToggle={() => toggleSection("execution")}
                >
                  <FormRow label="Working directory">
                    <DirField
                      value={draft.working_directory}
                      onChange={(working_directory) => edit({ working_directory })}
                      title="Choose working directory"
                      placeholder="Agent default"
                      browse={picker.browse}
                    />
                  </FormRow>
                  <FormRow label="Model">
                    <label className="field">
                      <input
                        value={draft.model}
                        onChange={(e) => edit({ model: e.target.value })}
                        placeholder={modelPlaceholder(draft.agent_slug, agents, defaultModel)}
                        spellCheck={false}
                      />
                    </label>
                  </FormRow>
                  <FormRow label="Timeout" help="Runs longer than this are cancelled.">
                    <div className="inline">
                      <label className="field field--num">
                        <input
                          type="number"
                          min={1}
                          value={draft.timeout_minutes || ""}
                          onChange={(e) =>
                            edit({ timeout_minutes: Number(e.target.value) || 0 })
                          }
                        />
                      </label>
                      <span className="inline__label">minutes</span>
                    </div>
                  </FormRow>
                  <FormRow label="Save output" help="Keep the agent's reply in the run history.">
                    <Switch
                      on={draft.save_output}
                      onChange={(v) => edit({ save_output: v })}
                    />
                  </FormRow>
                </FormSection>

                <div className="divider" />

                <FormSection
                  title="Limits"
                  summary={limitsSummary(draft)}
                  collapsible
                  open={isOpen("limits")}
                  onToggle={() => toggleSection("limits")}
                >
                  {/* Both rows are a mode switch over one stored value, and
                      the *unset* value is never rendered as a value: a `0` in
                      a spinner reads as "zero runs" and an empty
                      `datetime-local` paints the browser's own
                      `dd/mm/yyyy, --:--` mask, each the inverse of the "no
                      limit" it means. The segmented pair is also the clear
                      control the picker never had — selecting "No end date"
                      writes `null`, so an end date can be undone without
                      recreating the task. The wire is untouched. */}
                  <FormRow
                    label="Stop after"
                    help={
                      draft.stop_after_count > 0
                        ? "The task pauses itself once it has run this many times."
                        : "The number of runs is not limited."
                    }
                  >
                    <div className="inline">
                      <Segmented
                        value={draft.stop_after_count > 0 ? "count" : "none"}
                        options={[
                          { value: "none", label: "No limit" },
                          { value: "count", label: "Run limit" },
                        ]}
                        onChange={(v) => {
                          /* `Segmented` fires on every click, including one on
                             the option already selected — and both arms below
                             write. Without this guard the ordinary gesture of
                             re-clicking the mode you are already in would
                             re-seed a typed count, or stash the unset value
                             over the one the stash exists to remember. */
                          if (v === (draft.stop_after_count > 0 ? "count" : "none"))
                            return;
                          if (v === "none") {
                            lastLimits.current = {
                              ...stashFor(lastLimits.current, draft.id),
                              count: draft.stop_after_count,
                            };
                            edit({ stop_after_count: 0 });
                            return;
                          }
                          const kept = stashFor(lastLimits.current, draft.id).count;
                          edit({
                            stop_after_count:
                              kept > 0 ? kept : firstStopAfterCount(draft.run_count),
                          });
                        }}
                      />
                      {draft.stop_after_count > 0 && (
                        <>
                          <label className="field field--num">
                            {/* The floor is 1, not 0: `0` is now reached
                                through "No limit" alone, and letting the
                                number fall to it would collapse the input the
                                user is typing into. */}
                            <input
                              type="number"
                              min={1}
                              value={draft.stop_after_count}
                              onChange={(e) =>
                                edit({
                                  stop_after_count: Math.max(
                                    1,
                                    Number(e.target.value) || 1
                                  ),
                                })
                              }
                            />
                          </label>
                          <span className="inline__label">runs</span>
                        </>
                      )}
                    </div>
                  </FormRow>
                  <FormRow
                    label="Stop at"
                    help={
                      draft.stop_after_time
                        ? "Local time; stored as UTC."
                        : "The task has no end date."
                    }
                  >
                    <div className="inline">
                      <Segmented
                        value={draft.stop_after_time ? "date" : "none"}
                        options={[
                          { value: "none", label: "No end date" },
                          { value: "date", label: "End date" },
                        ]}
                        onChange={(v) => {
                          // The same already-selected guard as the row above.
                          if (v === (draft.stop_after_time ? "date" : "none")) return;
                          if (v === "none") {
                            lastLimits.current = {
                              ...stashFor(lastLimits.current, draft.id),
                              time: draft.stop_after_time ?? null,
                            };
                            edit({ stop_after_time: null });
                            return;
                          }
                          const kept = stashFor(lastLimits.current, draft.id).time;
                          edit({ stop_after_time: kept ?? firstStopAfterTime() });
                        }}
                      />
                      {draft.stop_after_time && (
                        <label className="field field--stamp">
                          <input
                            type="datetime-local"
                            value={toLocalStamp(draft.stop_after_time)}
                            onChange={(e) => {
                              /* An empty value is an edit in progress, not a
                                 clear. A `datetime-local` reports `""` for
                                 anything that is not a *complete* datetime, so
                                 backspacing one segment to retype it would
                                 otherwise write `null`, unmount this input
                                 under the user and lose the date — the same
                                 collapse the count input is floored against.
                                 "No end date" beside it is the clear, and the
                                 only one. `fromLocalStamp`'s own unparseable
                                 `""` folds in here too: `""` is not "no end
                                 date" to the API. */
                              const iso = fromLocalStamp(e.target.value);
                              if (iso) edit({ stop_after_time: iso });
                            }}
                          />
                        </label>
                      )}
                    </div>
                  </FormRow>
                </FormSection>

                <div className="divider" />

                {/* Instruction is last so it can take the pane's remaining
                    height; the flex chain is in tasks.css (#632). */}
                <div className="formsec tasks-form__fill">
                  <div className="formsec__title">Instruction</div>
                  <FormRow
                    label="Prompt"
                    help="Sent to the agent verbatim on every run."
                    className="tasks-form__fillrow"
                  >
                    <textarea
                      className="field-area"
                      rows={6}
                      value={draft.prompt}
                      onChange={(e) => edit({ prompt: e.target.value })}
                      spellCheck={false}
                    />
                  </FormRow>
                </div>
              </div>
            </div>
          </>
        )}
      </div>

      {inspectorOpen && (
        <>
          <Splitter variable="--inspector-w" min={220} max={420} invert />
          <aside className="pane-inspector">
            <div className="inspector__head">Task</div>
            <div className="inspector__scroll scroll">
              {draft && creating ? (
                <TaskPreviewBody
                  draft={draft}
                  preview={previewRes.data ?? null}
                  loading={previewRes.loading && !previewRes.data}
                  error={previewRes.error}
                />
              ) : !draft ? (
                <div className="statepane">Nothing selected</div>
              ) : (
                <>
                  <InspGroup title="Timing">
                    <InspRow label="Next run">
                      {draft.status === "active" ? (
                        <span title={dateTime(draft.next_run_at)}>
                          {draft.next_run_at ? relativeTime(draft.next_run_at) : "—"}
                        </span>
                      ) : (
                        "Paused"
                      )}
                    </InspRow>
                    <InspRow label="Last run">
                      <span title={dateTime(draft.last_run_at)}>
                        {draft.last_run_at ? relativeTime(draft.last_run_at) : "Never"}
                      </span>
                    </InspRow>
                    <InspRow label="Repeat">{describeSchedule(draft)}</InspRow>
                  </InspGroup>

                  <InspGroup title="Reliability">
                    <InspRow label="Total runs">
                      <span className="tnum">{draft.run_count}</span>
                    </InspRow>
                    <InspRow label="Last outcome">
                      <StatusBadge status={draft.last_run_status} />
                    </InspRow>
                    <InspRow label="Status">
                      <span
                        className={`badge ${
                          draft.status === "active" ? "badge--accent" : ""
                        }`}
                      >
                        {draft.status === "active" ? "Active" : "Paused"}
                      </span>
                    </InspRow>
                  </InspGroup>

                  <InspGroup title="Recent runs">
                    <RecentRuns
                      loading={historyRes.loading && !historyRes.data}
                      error={historyRes.error}
                      runs={historyRes.data ?? []}
                      onRetry={historyRes.reload}
                      onOpen={(jobId) => onNavigate("jobs", { jobId })}
                    />
                  </InspGroup>
                </>
              )}
            </div>
          </aside>
        </>
      )}
    </div>
  );
}

/* --- Create-mode preview (#633) ------------------------------------------ */

/** Where the run's model comes from, as a suffix; `agent_missing` is its own row. */
const MODEL_SOURCE: Record<TaskPreview["model_source"], string> = {
  agent: " · from agent",
  agent_missing: "",
  task: "",
  settings: " · from Settings",
  cli_default: "",
};

/**
 * What an unsaved task will do: its next fires, and what it will run as. Every
 * value comes from `POST /api/tasks/preview`, which uses the scheduler's and
 * the executor's own rules — see `preview_task` in `native/tasks.rs`.
 */
function TaskPreviewBody({
  draft,
  preview,
  loading,
  error,
}: {
  draft: ScheduledTask;
  preview: TaskPreview | null;
  loading: boolean;
  error?: string;
}) {
  const runs = preview?.next_runs ?? [];
  return (
    <>
      <InspGroup title="Schedule">
        <InspRow label="Repeat">{describeSchedule(draft)}</InspRow>
        {error ? (
          <div className="formerror">{error}</div>
        ) : loading || !preview ? (
          <InspRow label="Next runs">…</InspRow>
        ) : preview.schedule_error ? (
          <div className="formerror">{preview.schedule_error}</div>
        ) : draft.schedule_type === "run_immediately" ? (
          <InspRow label="Next run">As soon as it is saved</InspRow>
        ) : runs.length === 0 ? (
          <InspRow label="Next runs">None before the limits</InspRow>
        ) : (
          runs.map((t, i) => (
            <InspRow key={t} label={i === 0 ? "Next runs" : ""}>
              <span className="tnum" title={relativeTime(t)}>
                {dateTime(t)}
              </span>
            </InspRow>
          ))
        )}
      </InspGroup>

      {preview && (
        <InspGroup title="Runs as">
          <InspRow label="Model">
            {preview.model_source === "agent_missing"
              ? "Agent not found"
              : `${preview.model || "Claude Code default"}${MODEL_SOURCE[preview.model_source]}`}
          </InspRow>
          <InspRow label="Working dir">
            <span className="row insp-row__copy">
              <span
                className="mono truncate"
                title={preview.working_directory}
              >
                {preview.working_directory}
              </span>
              <CopyButton
                text={preview.working_directory}
                title="Copy the working directory"
              />
            </span>
          </InspRow>
          {preview.working_directory_source === "settings" && (
            <InspRow label="">from Settings</InspRow>
          )}
        </InspGroup>
      )}
    </>
  );
}

/* --- List row ------------------------------------------------------------- */

function TaskRow({
  t,
  active,
  onSelect,
}: {
  t: ScheduledTask;
  active: boolean;
  onSelect(id: string): void;
}) {
  return (
    <button
      className={`listrow ${active ? "listrow--active" : ""}`}
      onClick={() => onSelect(t.id)}
    >
      <div className={`avatar avatar--${toneFor(t.agent_slug)}`}>
        <Icon name="task" size={15} />
      </div>
      <div className="listrow__body">
        <div className="listrow__top">
          <span className="listrow__title">{t.name || "Untitled task"}</span>
          <span className="listrow__time">{relativeTime(t.last_run_at)}</span>
        </div>
        <div className="listrow__preview">
          {t.agent_slug || "no agent"} · {describeSchedule(t)}
        </div>
        <div className="listrow__meta">
          <StatusBadge status={t.last_run_status} />
          <span>
            {t.status === "active"
              ? t.next_run_at
                ? `next ${relativeTime(t.next_run_at)}`
                : "not scheduled"
              : "paused"}
          </span>
        </div>
      </div>
    </button>
  );
}

/* --- Recent runs (inspector) ---------------------------------------------- */

/**
 * The task's last few runs — and, since #542, the way to one of them.
 *
 * A run *is* a record with a detail view: the Jobs section renders its output,
 * error, timing, token counts and the Claude session it produced. Until this
 * was a control the only route there was to leave the task, open Job History
 * and hunt for the run by timestamp, which is the one thing anybody wants from
 * a scheduled task that failed last night.
 *
 * The rows stay in `base.css`'s text-selection denylist — `button` is on that
 * list already, so making the row a real button is what keeps a drag across it
 * from leaving a word highlighted behind the view it just opened (#469). Do
 * not add a per-view `user-select` here.
 */
function RecentRuns({
  loading,
  error,
  runs,
  onRetry,
  onOpen,
}: {
  loading: boolean;
  error: string | undefined;
  runs: JobHistory[];
  onRetry(): void;
  /** Hand this run over to the Jobs section. */
  onOpen(jobId: string): void;
}) {
  if (loading) return <div className="runrow">Loading…</div>;
  if (error) {
    return (
      <div className="runlist">
        <div className="formerror">{error}</div>
        <button className="btn btn--ghost" onClick={onRetry}>
          <Icon name="refresh" size={13} />
          Retry
        </button>
      </div>
    );
  }
  if (runs.length === 0) {
    return <div className="runrow">No runs yet.</div>;
  }
  return (
    <div className="runlist">
      {runs.slice(0, 8).map((j) => (
        <button
          type="button"
          className="runrow runrow--link"
          key={j.id}
          title={`${dateTime(j.started_at)} — open this run`}
          onClick={() => onOpen(j.id)}
        >
          <span className={`dot ${statusDot(j.status)}`} />
          <span className="runrow__when">{relativeTime(j.started_at)}</span>
          <span className="runrow__val">
            {j.status === "running" ? "running" : duration(j.duration_ms)}
          </span>
        </button>
      ))}
    </div>
  );
}

function statusDot(status: string): string {
  if (status === "running") return "dot--green dot--pulse";
  if (status === "success") return "dot--green";
  if (status === "failed") return "dot--red";
  return "dot--idle";
}

/* --- Agent picker --------------------------------------------------------- */

function AgentPicker({
  value,
  agents,
  onChange,
}: {
  value: string;
  agents: Agent[];
  onChange(slug: string): void;
}) {
  // A free-text slug box was the old answer here, on the reasoning that a task
  // whose agent was removed must stay repointable. It reads as "type the slug
  // from memory", which is not something anyone can do — and the case it was
  // for is covered by the `(missing)` entry below, which keeps an unknown slug
  // selectable inside the dropdown. "No agent" (the empty slug) is always the
  // first option and the default for a new task: the executor then runs Claude
  // Code with a stand-in agent (`resolve_agent` in `schedule/executor.rs`), so
  // the picker renders even with zero agents rather than blocking the form.
  const known = agents.some((a) => a.slug === value);
  return (
    <Picker
      value={value}
      onChange={onChange}
      options={[
        { value: "", label: "No agent" },
        ...(value && !known ? [{ value, label: `${value} (missing)` }] : []),
        ...agents.map((a) => ({ value: a.slug, label: a.name || a.slug })),
      ]}
    />
  );
}

function Picker({
  value,
  options,
  onChange,
  small,
}: {
  value: string;
  options: { value: string; label: string }[];
  onChange(v: string): void;
  small?: boolean;
}) {
  return (
    <Dropdown value={value} options={options} onChange={onChange} small={small} />
  );
}

/* --- Schedule editor ------------------------------------------------------ */

function ScheduleEditor({
  type,
  config,
  onChange,
}: {
  type: ScheduleType;
  config: ScheduleConfig;
  onChange(type: ScheduleType, config: ScheduleConfig): void;
}) {
  const unit = intervalUnit(config);
  const count = intervalCount(config);

  return (
    <>
      <FormRow label="Repeat">
        <Segmented<ScheduleType>
          value={type}
          options={SCHEDULE_OPTIONS}
          onChange={(next) => onChange(next, configForType(next, config))}
        />
      </FormRow>

      {type === "run_immediately" && (
        <FormRow label="When">
          <div className="formrow__help">
            Runs once, as soon as the task is saved. No schedule is kept.
          </div>
        </FormRow>
      )}

      {type === "one_off" && (
        <FormRow label="Run at" help="Local time; stored as UTC.">
          <label className="field field--stamp">
            <input
              type="datetime-local"
              value={toLocalStamp(config.run_at)}
              onChange={(e) =>
                onChange("one_off", { run_at: fromLocalStamp(e.target.value) })
              }
            />
          </label>
        </FormRow>
      )}

      {type === "interval" && (
        <>
          <FormRow label="Every">
            <div className="inline">
              <label className="field field--num">
                <input
                  type="number"
                  min={1}
                  value={count}
                  onChange={(e) =>
                    onChange(
                      "interval",
                      intervalConfig(unit, Number(e.target.value), config.at_time ?? "")
                    )
                  }
                />
              </label>
              <Picker
                value={unit}
                options={[
                  { value: "minutes", label: "minutes" },
                  { value: "hours", label: "hours" },
                  { value: "days", label: "days" },
                ]}
                onChange={(next) =>
                  onChange(
                    "interval",
                    intervalConfig(next as IntervalUnit, count, config.at_time ?? "")
                  )
                }
              />
            </div>
          </FormRow>
          {unit === "days" && (
            <FormRow label="At" help="Time of day for the daily run. Optional.">
              <label className="field field--time">
                <input
                  type="time"
                  value={config.at_time ?? ""}
                  onChange={(e) =>
                    onChange("interval", intervalConfig(unit, count, e.target.value))
                  }
                />
              </label>
            </FormRow>
          )}
        </>
      )}

      {type === "cron" && (
        <FormRow label="Expression" help="minute hour day-of-month month day-of-week">
          <label className="field">
            <input
              className="mono"
              value={config.expression ?? ""}
              onChange={(e) => onChange("cron", { expression: e.target.value })}
              placeholder="0 9 * * *"
              spellCheck={false}
            />
          </label>
        </FormRow>
      )}
    </>
  );
}

/* --- Shared outcome badge (also used by JobsView) ------------------------- */

export function StatusBadge({ status }: { status?: string | null }) {
  if (!status) return <span className="badge">Never run</span>;
  if (status === "running") {
    return (
      <span className="badge badge--green">
        <span className="dot dot--green dot--pulse" />
        Running
      </span>
    );
  }
  if (status === "success") return <span className="badge badge--green">Success</span>;
  if (status === "failed") return <span className="badge badge--red">Failed</span>;
  return <span className="badge">{status}</span>;
}
