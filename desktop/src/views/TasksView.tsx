import { useEffect, useMemo, useState } from "react";
import { api } from "../lib/api";
import type {
  Agent,
  JobHistory,
  ScheduleConfig,
  ScheduleType,
  ScheduledTask,
} from "../lib/types";
import { describeError, usePoll, useResource } from "../lib/hooks";
import { dateTime, duration, relativeTime, toneFor } from "../lib/format";
import { Icon } from "../lib/icons";
import {
  Empty,
  FormRow,
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

/* --- New-task template ---------------------------------------------------- */

function blankTask(agentSlug: string): ScheduledTask {
  const now = new Date().toISOString();
  return {
    id: "",
    name: "New task",
    description: "",
    prompt: "",
    agent_slug: agentSlug,
    working_directory: "",
    model: "",
    settings_profile_id: "",
    timeout_minutes: 30,
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

/* --- View ----------------------------------------------------------------- */

export function TasksView({ inspectorOpen }: { inspectorOpen: boolean }) {
  const tasksRes = useResource<ScheduledTask[] | null>(
    (signal) => api.get("/tasks", signal),
    []
  );
  const agentsRes = useResource<Agent[] | null>(
    (signal) => api.get("/agents", signal),
    []
  );

  const tasks = useMemo(() => tasksRes.data ?? [], [tasksRes.data]);
  const agents = useMemo(() => agentsRes.data ?? [], [agentsRes.data]);

  usePoll(tasksRes.reload, POLL_MS);

  const [query, setQuery] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [draft, setDraft] = useState<ScheduledTask | null>(null);
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<string>();
  const [confirmDelete, setConfirmDelete] = useState(false);

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

  function edit(patch: Partial<ScheduledTask>) {
    setDraft((d) => (d ? { ...d, ...patch } : d));
    setDirty(true);
  }

  function startCreate() {
    setCreating(true);
    setConfirmDelete(false);
    setActionError(undefined);
    setDraft(blankTask(agents[0]?.slug ?? ""));
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
    setDirty(false);
    setActionError(undefined);
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
                  <span className="confirm__text">Delete this task and its history?</span>
                  <button
                    className="btn btn--ghost"
                    onClick={() => setConfirmDelete(false)}
                  >
                    Cancel
                  </button>
                  <button className="btn btn--danger" onClick={remove} disabled={busy}>
                    Delete
                  </button>
                </div>
              ) : (
                <>
                  {(dirty || creating) && (
                    <button className="btn btn--ghost" onClick={revert} disabled={busy}>
                      {creating ? "Discard" : "Revert"}
                    </button>
                  )}
                  <button
                    className="btn btn--primary"
                    onClick={save}
                    disabled={busy || (!dirty && !creating)}
                  >
                    {creating ? "Create" : "Save"}
                  </button>
                  {!creating && (
                    <>
                      <div className="toolbar__sep" />
                      <button
                        className="iconbtn"
                        title={draft.status === "active" ? "Pause" : "Resume"}
                        onClick={toggleStatus}
                        disabled={busy}
                      >
                        <Icon
                          name={draft.status === "active" ? "pause" : "play"}
                          size={14}
                        />
                      </button>
                      <button
                        className="iconbtn"
                        title="Delete"
                        onClick={() => setConfirmDelete(true)}
                      >
                        <Icon name="trash" size={14} />
                      </button>
                    </>
                  )}
                </>
              )}
            </div>

            <div className="scroll" style={{ flex: 1, padding: "var(--sp-8)" }}>
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
                  <FormRow label="Agent">
                    <AgentPicker
                      value={draft.agent_slug}
                      agents={agents}
                      loading={agentsRes.loading && !agentsRes.data}
                      onChange={(slug) => edit({ agent_slug: slug })}
                    />
                  </FormRow>
                  {!creating && (
                    <FormRow
                      label="Enabled"
                      help="Paused tasks keep their history but never fire."
                    >
                      <Switch on={draft.status === "active"} onChange={toggleStatus} />
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

                <div className="formsec">
                  <div className="formsec__title">Instruction</div>
                  <FormRow label="Prompt" help="Sent to the agent verbatim on every run.">
                    <textarea
                      className="field-area"
                      rows={6}
                      value={draft.prompt}
                      onChange={(e) => edit({ prompt: e.target.value })}
                      spellCheck={false}
                    />
                  </FormRow>
                </div>

                <div className="divider" />

                <div className="formsec">
                  <div className="formsec__title">Execution</div>
                  <FormRow label="Working directory">
                    <label className="field">
                      <input
                        value={draft.working_directory}
                        onChange={(e) => edit({ working_directory: e.target.value })}
                        placeholder="Agent default"
                        spellCheck={false}
                      />
                    </label>
                  </FormRow>
                  <FormRow label="Model">
                    <label className="field">
                      <input
                        value={draft.model}
                        onChange={(e) => edit({ model: e.target.value })}
                        placeholder="Agent default"
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
                </div>

                <div className="divider" />

                <div className="formsec">
                  <div className="formsec__title">Limits</div>
                  <FormRow label="Stop after" help="0 keeps the task running forever.">
                    <div className="inline">
                      <label className="field field--num">
                        <input
                          type="number"
                          min={0}
                          value={draft.stop_after_count}
                          onChange={(e) =>
                            edit({ stop_after_count: Number(e.target.value) || 0 })
                          }
                        />
                      </label>
                      <span className="inline__label">runs</span>
                    </div>
                  </FormRow>
                  <FormRow label="Stop at" help="Leave empty for no end date.">
                    <label className="field field--stamp">
                      <input
                        type="datetime-local"
                        value={toLocalStamp(draft.stop_after_time)}
                        onChange={(e) =>
                          edit({
                            stop_after_time: e.target.value
                              ? fromLocalStamp(e.target.value)
                              : null,
                          })
                        }
                      />
                    </label>
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
              {!draft || creating ? (
                <div className="statepane">
                  {creating ? "Unsaved task" : "Nothing selected"}
                </div>
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

function RecentRuns({
  loading,
  error,
  runs,
  onRetry,
}: {
  loading: boolean;
  error: string | undefined;
  runs: JobHistory[];
  onRetry(): void;
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
        <div className="runrow" key={j.id} title={dateTime(j.started_at)}>
          <span className={`dot ${statusDot(j.status)}`} />
          <span className="runrow__when">{relativeTime(j.started_at)}</span>
          <span className="runrow__val">
            {j.status === "running" ? "running" : duration(j.duration_ms)}
          </span>
        </div>
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
  loading,
  onChange,
}: {
  value: string;
  agents: Agent[];
  loading: boolean;
  onChange(slug: string): void;
}) {
  // With no agents to choose from the slug still has to be editable, or a task
  // whose agent was removed could never be pointed at a new one.
  if (!loading && agents.length === 0) {
    return (
      <label className="field">
        <input
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder="agent-slug"
          spellCheck={false}
        />
      </label>
    );
  }

  const known = agents.some((a) => a.slug === value);
  return (
    <Picker
      value={value}
      onChange={onChange}
      options={[
        ...(value && !known ? [{ value, label: `${value} (missing)` }] : []),
        ...(value ? [] : [{ value: "", label: "Select an agent…" }]),
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
    <div className={`selectwrap ${small ? "selectwrap--sm" : ""}`}>
      <select
        className="selectfield"
        value={value}
        onChange={(e) => onChange(e.target.value)}
      >
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
      <span className="select__chevron">
        <Icon name="chevronUD" size={12} />
      </span>
    </div>
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
