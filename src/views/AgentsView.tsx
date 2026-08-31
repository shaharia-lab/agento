import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../lib/api";
import { describeError, useResource } from "../lib/hooks";
import { DESTROY, DESTROYING } from "../lib/formVerbs";
import { SaveBar } from "../components/SaveBar";
import { initials, toneFor } from "../lib/format";
import { Icon } from "../lib/icons";
import type {
  Agent,
  AgentCapabilities,
  AvailableTool,
  SettingsResponse,
} from "../lib/types";
import {
  Checkbox,
  Dropdown,
  Empty,
  FormRow,
  InspGroup,
  InspRow,
  Search,
  Splitter,
} from "../components/ui";
import { DirField, useDirPicker } from "../components/DirField";
import "../styles/agents.css";

/* --- Vocabulary the backend recognises ------------------------------------ */

const BUILT_IN_TOOLS = [
  "Read",
  "Write",
  "Edit",
  "Bash",
  "Glob",
  "Grep",
  "WebFetch",
  "WebSearch",
  "Task",
  "TaskOutput",
  "TaskStop",
  "NotebookEdit",
];

const LOCAL_TOOLS = ["current_time"];

interface Option {
  value: string;
  label: string;
}

const MODELS: Option[] = [
  { value: "sonnet", label: "Sonnet" },
  { value: "opus", label: "Opus" },
  { value: "haiku", label: "Haiku" },
];

const THINKING: Option[] = [
  { value: "adaptive", label: "Adaptive" },
  { value: "enabled", label: "Always on" },
  { value: "disabled", label: "Disabled" },
];

const PERMISSION_MODES: Option[] = [
  { value: "bypass", label: "Bypass — never ask" },
  { value: "default", label: "Default — ask before acting" },
  { value: "plan", label: "Plan — propose, do not act" },
  { value: "dontAsk", label: "Don't ask" },
];

const PERMISSION_LABELS: Record<string, string> = {
  bypass: "Bypass",
  default: "Default",
  plan: "Plan",
  dontAsk: "Don't ask",
};

const EMPTY_CAPS: AgentCapabilities = { built_in: null, local: null, mcp: null };

/* --- Component ------------------------------------------------------------ */

export function AgentsView({ inspectorOpen }: { inspectorOpen: boolean }) {
  const list = useResource<Agent[]>((s) => api.get<Agent[]>("/agents", s), []);
  const available = useResource<AvailableTool[]>(
    (s) => api.get<AvailableTool[]>("/integrations/available-tools", s),
    []
  );
  // GET /settings answers an envelope, not the settings object itself.
  const settings = useResource<SettingsResponse>(
    (s) => api.get<SettingsResponse>("/settings", s),
    []
  );

  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [draft, setDraft] = useState<Agent | null>(null);
  /** The last server state of the selected agent; null while creating. */
  const [baseline, setBaseline] = useState<Agent | null>(null);
  const [slugTouched, setSlugTouched] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string>();
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [busyDelete, setBusyDelete] = useState(false);
  const picker = useDirPicker();

  const agents = useMemo(() => list.data ?? [], [list.data]);

  const detail = useResource<Agent | null>(
    (s) =>
      selected === null
        ? Promise.resolve(null)
        : api.get<Agent>(`/agents/${encodeURIComponent(selected)}`, s),
    [selected]
  );

  // The detail response is the only writer of the baseline, so a save (which
  // sets both from its own response) is never clobbered by a stale fetch.
  useEffect(() => {
    const a = detail.data;
    if (!a || a.slug !== selected) return;
    setDraft(a);
    setBaseline(a);
  }, [detail.data, selected]);

  const autoPicked = useRef(false);
  useEffect(() => {
    if (autoPicked.current || creating || selected !== null) return;
    if (!agents.length) return;
    autoPicked.current = true;
    setSelected(agents[0].slug);
  }, [agents, creating, selected]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return agents;
    return agents.filter(
      (a) =>
        a.name.toLowerCase().includes(q) ||
        a.slug.toLowerCase().includes(q) ||
        (a.description ?? "").toLowerCase().includes(q)
    );
  }, [agents, query]);

  const selectAgent = useCallback((slug: string) => {
    setCreating(false);
    setSelected(slug);
    setDraft(null);
    setBaseline(null);
    setSlugTouched(false);
    setSaveError(undefined);
    setConfirmDelete(false);
  }, []);

  const startCreate = useCallback(() => {
    setCreating(true);
    setSelected(null);
    setBaseline(null);
    setSlugTouched(false);
    setSaveError(undefined);
    setConfirmDelete(false);
    setDraft(blankAgent(settings.data?.settings.default_model ?? "sonnet"));
  }, [settings.data]);

  const patch = useCallback((fields: Partial<Agent>) => {
    setDraft((d) => (d ? { ...d, ...fields } : d));
  }, []);

  const patchCaps = useCallback(
    (fn: (caps: AgentCapabilities) => AgentCapabilities) => {
      setDraft((d) =>
        d ? { ...d, capabilities: fn(d.capabilities ?? EMPTY_CAPS) } : d
      );
    },
    []
  );

  const dirty =
    draft !== null && (baseline === null || canonical(draft) !== canonical(baseline));

  async function save() {
    if (!draft || saving) return;
    setSaving(true);
    setSaveError(undefined);
    try {
      const body = payload(draft);
      const saved = creating
        ? await api.post<Agent>("/agents", body)
        : await api.put<Agent>(
            `/agents/${encodeURIComponent(draft.slug)}`,
            body
          );
      setCreating(false);
      setSlugTouched(false);
      setDraft(saved);
      setBaseline(saved);
      setSelected(saved.slug);
      autoPicked.current = true;
      list.reload();
    } catch (err) {
      setSaveError(describeError(err));
    } finally {
      setSaving(false);
    }
  }

  function revert() {
    setSaveError(undefined);
    if (creating) {
      setCreating(false);
      setDraft(null);
      setBaseline(null);
      return;
    }
    setDraft(baseline);
  }

  async function remove() {
    if (!baseline || busyDelete) return;
    setBusyDelete(true);
    try {
      await api.del<void>(`/agents/${encodeURIComponent(baseline.slug)}`);
      setConfirmDelete(false);
      setSelected(null);
      setDraft(null);
      setBaseline(null);
      list.reload();
    } catch (err) {
      setSaveError(describeError(err));
      setConfirmDelete(false);
    } finally {
      setBusyDelete(false);
    }
  }

  const caps = draft?.capabilities ?? EMPTY_CAPS;
  const builtIn = caps.built_in ?? [];
  const local = caps.local ?? [];
  const mcp = caps.mcp ?? {};
  const mcpToolCount = Object.values(mcp).reduce(
    (n, cap) => n + (cap?.tools?.length ?? 0),
    0
  );

  const groups = useMemo(
    () => toolGroups(available.data ?? [], mcp),
    [available.data, mcp]
  );

  const detailBusy = selected !== null && detail.loading && draft === null;

  return (
    <div className="panes">
      {picker.browser}
      <div className="pane-list">
        <div className="listhead">
          <div className="listhead__row">
            <Search value={query} onChange={setQuery} placeholder="Search agents" />
            <button className="iconbtn" title="New Agent" onClick={startCreate}>
              <Icon name="plus" size={15} />
            </button>
          </div>
        </div>

        <div className="list__scroll scroll">
          {creating && draft && (
            <button className="listrow listrow--active">
              <div className={`avatar avatar--${toneFor(draft.slug || "new")}`}>
                {initials(draft.name || "New")}
              </div>
              <div className="listrow__body">
                <div className="listrow__top">
                  <span className="listrow__title">
                    {draft.name || "New agent"}
                  </span>
                  <span className="dot dot--amber" />
                </div>
                <div className="listrow__preview">Unsaved draft</div>
                <div className="listrow__meta">
                  <span>{draft.model}</span>
                </div>
              </div>
            </button>
          )}

          {list.loading && !list.data && (
            <div className="agents-note">Loading agents…</div>
          )}

          {list.error && (
            <div className="agents-note">
              <span>{list.error}</span>
              <button className="btn" onClick={list.reload}>
                <Icon name="refresh" size={13} />
                Retry
              </button>
            </div>
          )}

          {!list.error &&
            filtered.map((a) => (
              <button
                key={a.slug}
                className={`listrow ${
                  a.slug === selected && !creating ? "listrow--active" : ""
                }`}
                onClick={() => selectAgent(a.slug)}
              >
                <div className={`avatar avatar--${toneFor(a.slug)}`}>
                  {initials(a.name)}
                </div>
                <div className="listrow__body">
                  <div className="listrow__top">
                    <span className="listrow__title">{a.name}</span>
                    {a.slug === selected && dirty && !creating && (
                      <span className="dot dot--amber" />
                    )}
                  </div>
                  <div className="listrow__preview">
                    {a.description || "No description"}
                  </div>
                  <div className="listrow__meta">
                    <span>{a.model}</span>
                    <span>·</span>
                    <span>{permissionLabel(a.permission_mode)}</span>
                  </div>
                </div>
              </button>
            ))}

          {!list.loading && !list.error && !filtered.length && query && (
            <div className="agents-note">No agents match “{query}”.</div>
          )}
        </div>
      </div>

      <Splitter variable="--list-w" min={240} max={460} />

      <div className="pane-detail">
        {!draft ? (
          detailBusy ? (
            <Empty icon="agent" title="Loading…" text="Fetching the agent." />
          ) : detail.error ? (
            <Empty
              icon="alert"
              title="Could not load this agent"
              text={detail.error}
              action={
                <button className="btn btn--lg" onClick={detail.reload}>
                  <Icon name="refresh" size={13} />
                  Retry
                </button>
              }
            />
          ) : list.error ? (
            <Empty
              icon="alert"
              title="Could not load agents"
              text={list.error}
              action={
                <button className="btn btn--lg" onClick={list.reload}>
                  <Icon name="refresh" size={13} />
                  Retry
                </button>
              }
            />
          ) : list.loading ? (
            <Empty icon="agent" title="Loading…" text="Fetching your agents." />
          ) : agents.length ? (
            <Empty
              icon="agent"
              title="No agent selected"
              text="Select an agent to edit its instructions, model and tools."
            />
          ) : (
            <Empty
              icon="agent"
              title="No agents yet"
              text="An agent is a named Claude Code configuration — a model, a system prompt and the tools it is allowed to use."
              action={
                <button className="btn btn--lg btn--primary" onClick={startCreate}>
                  <Icon name="plus" size={13} />
                  New Agent
                </button>
              }
            />
          )
        ) : (
          <>
            <div className="toolbar">
              <div className="toolbar__title">
                {draft.name || (creating ? "New agent" : draft.slug)}
              </div>
              {creating ? (
                <span className="badge badge--amber">draft</span>
              ) : (
                <span className="badge">{permissionLabel(draft.permission_mode)}</span>
              )}
              <div className="spacer" />
              {!creating && (
                <button
                  className="iconbtn"
                  title={DESTROY}
                  onClick={() => setConfirmDelete(true)}
                >
                  <Icon name="trash" size={14} />
                </button>
              )}
            </div>

            {confirmDelete && baseline && (
              <div className="agents-confirm">
                <span className="agents-confirm__icon">
                  <Icon name="alert" size={15} />
                </span>
                <span style={{ flex: 1 }}>
                  {DESTROY} <strong>{baseline.name}</strong>? Its configuration
                  goes with it.
                </span>
                <button className="btn" onClick={() => setConfirmDelete(false)}>
                  Cancel
                </button>
                <button
                  className="btn btn--danger"
                  disabled={busyDelete}
                  onClick={remove}
                >
                  {busyDelete ? DESTROYING : DESTROY}
                </button>
              </div>
            )}

            <div className="scroll" style={{ flex: 1, padding: "var(--sp-8)" }}>
              <div className="form">
                <div className="formsec">
                  <div className="formsec__title">Identity</div>
                  <FormRow label="Name">
                    <label className="field">
                      <input
                        value={draft.name}
                        placeholder="Release Notes Writer"
                        onChange={(e) => {
                          const name = e.target.value;
                          patch(
                            creating && !slugTouched
                              ? { name, slug: slugify(name) }
                              : { name }
                          );
                        }}
                      />
                    </label>
                  </FormRow>
                  <FormRow
                    label="Slug"
                    help={
                      creating
                        ? "Lowercase letters, digits and hyphens. Permanent once created."
                        : "The stable identifier — it cannot be changed after creation."
                    }
                  >
                    {creating ? (
                      <label className="field">
                        <input
                          className="mono"
                          value={draft.slug}
                          placeholder="release-notes-writer"
                          spellCheck={false}
                          onChange={(e) => {
                            setSlugTouched(true);
                            patch({ slug: e.target.value });
                          }}
                        />
                      </label>
                    ) : (
                      <span
                        className="mono"
                        style={{ color: "var(--fg-secondary)", paddingTop: 6 }}
                      >
                        {draft.slug}
                      </span>
                    )}
                  </FormRow>
                  <FormRow
                    label="Description"
                    help="Shown in the agent list and used when Agento picks an agent for you."
                  >
                    <textarea
                      className="field-area"
                      rows={3}
                      value={draft.description}
                      onChange={(e) => patch({ description: e.target.value })}
                    />
                  </FormRow>
                </div>

                <div className="divider" />

                <div className="formsec">
                  <div className="formsec__title">Behaviour</div>
                  <FormRow label="Model">
                    <Picker
                      value={draft.model}
                      options={withCurrent(MODELS, draft.model)}
                      onChange={(model) => patch({ model })}
                    />
                  </FormRow>
                  <FormRow label="Thinking" help="How much extended reasoning the agent may spend.">
                    <Picker
                      value={draft.thinking}
                      options={withCurrent(THINKING, draft.thinking)}
                      onChange={(thinking) => patch({ thinking })}
                    />
                  </FormRow>
                  <FormRow
                    label="Permission mode"
                    help="How often the agent stops to ask before it acts."
                  >
                    <Picker
                      value={draft.permission_mode}
                      options={withCurrent(PERMISSION_MODES, draft.permission_mode)}
                      onChange={(permission_mode) => patch({ permission_mode })}
                    />
                  </FormRow>
                  <FormRow
                    label="System prompt"
                    help="Prepended to every conversation this agent takes part in."
                  >
                    <textarea
                      className="field-area mono"
                      rows={8}
                      value={draft.system_prompt}
                      onChange={(e) => patch({ system_prompt: e.target.value })}
                    />
                  </FormRow>
                  <FormRow
                    label="Claude config dir"
                    help="Absolute path. Leave empty to use the global default."
                  >
                    <DirField
                      value={draft.claude_config_dir}
                      onChange={(claude_config_dir) => patch({ claude_config_dir })}
                      title="Choose Claude config directory"
                      placeholder="/home/you/.claude"
                      browse={picker.browse}
                    />
                  </FormRow>
                </div>

                <div className="divider" />

                <div className="formsec">
                  <div className="formsec__title">Capabilities</div>

                  <FormRow
                    label="Built-in tools"
                    help={
                      builtIn.length === 0 && local.length === 0 && mcpToolCount === 0
                        ? "Nothing selected anywhere: the agent runs with every built-in tool. Select one to start restricting."
                        : "Only the selected tools are allowed; the rest are explicitly denied."
                    }
                  >
                    <div className="agents-caps">
                      {BUILT_IN_TOOLS.map((t) => (
                        <CapRow
                          key={t}
                          label={t}
                          on={builtIn.includes(t)}
                          onChange={(on) =>
                            patchCaps((c) => ({
                              ...c,
                              built_in: nonEmpty(toggle(c.built_in ?? [], t, on)),
                            }))
                          }
                        />
                      ))}
                    </div>
                  </FormRow>

                  <FormRow label="Local tools" help="Served in-process by Agento itself.">
                    <div className="agents-caps">
                      {LOCAL_TOOLS.map((t) => (
                        <CapRow
                          key={t}
                          label={t}
                          on={local.includes(t)}
                          onChange={(on) =>
                            patchCaps((c) => ({
                              ...c,
                              local: nonEmpty(toggle(c.local ?? [], t, on)),
                            }))
                          }
                        />
                      ))}
                    </div>
                  </FormRow>

                  <FormRow
                    label="Integration tools"
                    help="Grouped by integration. Each integration is exposed to the agent as an MCP server carrying only the tools ticked here."
                  >
                    {available.error ? (
                      <div className="agents-note" style={{ padding: 0 }}>
                        <span>{available.error}</span>
                        <button className="btn" onClick={available.reload}>
                          <Icon name="refresh" size={13} />
                          Retry
                        </button>
                      </div>
                    ) : available.loading && !available.data ? (
                      <div className="agents-hint">Loading integration tools…</div>
                    ) : !groups.length ? (
                      <div className="agents-hint">
                        No connected integration exposes tools yet. Connect one in
                        Integrations and it will appear here.
                      </div>
                    ) : (
                      <div>
                        {groups.map((g) => {
                          const picked = mcp[g.id]?.tools ?? [];
                          return (
                            <div className="agents-group" key={g.id}>
                              <div className="agents-group__head">
                                <Icon
                                  name="plug"
                                  size={14}
                                  style={{ color: "var(--fg-tertiary)" }}
                                />
                                <span className="agents-group__name">{g.name}</span>
                                <span className="agents-group__sub">
                                  {picked.length} of {g.tools.length}
                                </span>
                                <div className="spacer" />
                                <button
                                  className="btn"
                                  onClick={() =>
                                    patchCaps((c) =>
                                      setMcpTools(
                                        c,
                                        g.id,
                                        g.tools
                                          .filter((t) => t.available)
                                          .map((t) => t.name)
                                      )
                                    )
                                  }
                                >
                                  All
                                </button>
                                <button
                                  className="btn"
                                  onClick={() =>
                                    patchCaps((c) => setMcpTools(c, g.id, []))
                                  }
                                >
                                  None
                                </button>
                              </div>
                              <div className="agents-caps">
                                {g.tools.map((t) => (
                                  <CapRow
                                    key={t.name}
                                    label={t.name}
                                    title={
                                      t.available
                                        ? `mcp__${g.id}__${t.name}`
                                        : "No longer offered by this integration"
                                    }
                                    gone={!t.available}
                                    on={picked.includes(t.name)}
                                    onChange={(on) =>
                                      patchCaps((c) =>
                                        setMcpTools(
                                          c,
                                          g.id,
                                          toggle(
                                            c.mcp?.[g.id]?.tools ?? [],
                                            t.name,
                                            on
                                          )
                                        )
                                      )
                                    }
                                  />
                                ))}
                              </div>
                            </div>
                          );
                        })}
                      </div>
                    )}
                  </FormRow>
                </div>

                {/* The strip is inside `.form` rather than docked below the
                    scroll container, which is where every other savebar in the
                    app lives: `.savebar` is `position: sticky; bottom: 0`, so
                    it needs a scrolling ancestor to pin itself against (#519).

                    Its condition stays `dirty || saveError` — the one thing
                    this view's savebar does that the others' do not. A save
                    that failed is not a pending change, so gating on `dirty`
                    alone would leave the failure with nowhere to show. */}
                {(dirty || saveError) && (
                  <SaveBar
                    creating={creating}
                    busy={saving}
                    canSubmit={draft.name.trim() !== "" && dirty}
                    message={
                      saveError ??
                      (creating ? "New agent — not saved yet" : "Unsaved changes")
                    }
                    messageIcon={saveError ? "alert" : "edit"}
                    messageTone={saveError ? "error" : undefined}
                    onDiscard={revert}
                    onSubmit={save}
                  />
                )}
              </div>
            </div>
          </>
        )}
      </div>

      {inspectorOpen && draft && (
        <>
          <Splitter variable="--inspector-w" min={220} max={420} invert />
          <aside className="pane-inspector">
            <div className="inspector__head">Agent</div>
            <div className="inspector__scroll scroll">
              <InspGroup title="Identity">
                <InspRow label="Slug">
                  <span className="mono">
                    {draft.slug || (creating ? "—" : draft.slug)}
                  </span>
                </InspRow>
                <InspRow label="Model">{draft.model || "—"}</InspRow>
                <InspRow label="Thinking">{draft.thinking || "adaptive"}</InspRow>
                <InspRow label="Permission">
                  {permissionLabel(draft.permission_mode)}
                </InspRow>
              </InspGroup>

              <InspGroup title="Tools">
                <InspRow label="Built-in">
                  <span className="tnum">
                    {builtIn.length === 0 &&
                    local.length === 0 &&
                    mcpToolCount === 0
                      ? `all ${BUILT_IN_TOOLS.length}`
                      : `${builtIn.length} / ${BUILT_IN_TOOLS.length}`}
                  </span>
                </InspRow>
                <InspRow label="Local">
                  <span className="tnum">
                    {local.length} / {LOCAL_TOOLS.length}
                  </span>
                </InspRow>
                <InspRow label="Integration">
                  <span className="tnum">{mcpToolCount}</span>
                </InspRow>
                <InspRow label="Integrations">
                  <span className="tnum">{Object.keys(mcp).length}</span>
                </InspRow>
              </InspGroup>

              <InspGroup title="Runtime">
                <InspRow label="Config dir">
                  <span className="mono">
                    {draft.claude_config_dir || "Default"}
                  </span>
                </InspRow>
                <InspRow label="State">
                  {creating ? "Unsaved draft" : dirty ? "Edited" : "Saved"}
                </InspRow>
              </InspGroup>
            </div>
          </aside>
        </>
      )}
    </div>
  );
}

/* --- Local pieces --------------------------------------------------------- */

function Picker({
  value,
  options,
  onChange,
}: {
  value: string;
  options: Option[];
  onChange(v: string): void;
}) {
  return (
    <Dropdown
      value={value}
      options={options}
      onChange={onChange}
      className="agents-select"
    />
  );
}

/**
 * Checkbox and label are siblings rather than a wrapping label: Checkbox is a
 * <button>, which cannot be nested inside a clickable parent.
 */
function CapRow({
  label,
  on,
  onChange,
  title,
  gone,
}: {
  label: string;
  on: boolean;
  onChange(v: boolean): void;
  title?: string;
  gone?: boolean;
}) {
  return (
    <div className={`agents-cap ${gone ? "agents-cap--gone" : ""}`} title={title}>
      <Checkbox on={on} onChange={onChange} />
      <span className="agents-cap__label" onClick={() => onChange(!on)}>
        {label}
      </span>
    </div>
  );
}

/* --- Data helpers --------------------------------------------------------- */

interface ToolGroup {
  id: string;
  name: string;
  tools: { name: string; service: string; available: boolean }[];
}

/**
 * Group the catalogue by integration id — the key `capabilities.mcp` uses.
 * Integrations the agent still references but which no longer offer tools are
 * appended so a stale selection stays visible and removable.
 */
function toolGroups(
  tools: AvailableTool[],
  mcp: Record<string, { tools: string[] } | undefined>
): ToolGroup[] {
  const byId = new Map<string, ToolGroup>();

  for (const t of tools) {
    let g = byId.get(t.integration_id);
    if (!g) {
      g = { id: t.integration_id, name: t.integration_name, tools: [] };
      byId.set(t.integration_id, g);
    }
    if (!g.tools.some((x) => x.name === t.tool_name)) {
      g.tools.push({ name: t.tool_name, service: t.service, available: true });
    }
  }

  for (const [id, cap] of Object.entries(mcp)) {
    let g = byId.get(id);
    if (!g) {
      g = { id, name: id, tools: [] };
      byId.set(id, g);
    }
    for (const name of cap?.tools ?? []) {
      if (!g.tools.some((x) => x.name === name)) {
        g.tools.push({ name, service: "", available: false });
      }
    }
  }

  return [...byId.values()].sort((a, b) => a.name.localeCompare(b.name));
}

function setMcpTools(
  caps: AgentCapabilities,
  integrationId: string,
  tools: string[]
): AgentCapabilities {
  const next = { ...(caps.mcp ?? {}) };
  // An integration with no tools carries no meaning — drop the key entirely so
  // the agent does not reference an MCP server it never calls.
  if (tools.length) next[integrationId] = { tools };
  else delete next[integrationId];
  return { ...caps, mcp: Object.keys(next).length ? next : null };
}

function toggle(list: string[], value: string, on: boolean): string[] {
  if (on) return list.includes(value) ? list : [...list, value];
  return list.filter((v) => v !== value);
}

function nonEmpty(list: string[]): string[] | null {
  return list.length ? list : null;
}

function blankAgent(model: string): Agent {
  return {
    name: "",
    slug: "",
    description: "",
    model,
    thinking: "adaptive",
    permission_mode: "default",
    system_prompt: "",
    capabilities: { built_in: null, local: null, mcp: null },
    claude_config_dir: "",
  };
}

function payload(a: Agent): Agent {
  const c = a.capabilities ?? EMPTY_CAPS;
  return {
    ...a,
    name: a.name.trim(),
    slug: a.slug.trim(),
    claude_config_dir: a.claude_config_dir.trim(),
    capabilities: {
      built_in: nonEmpty(c.built_in ?? []),
      local: nonEmpty(c.local ?? []),
      mcp: c.mcp && Object.keys(c.mcp).length ? c.mcp : null,
    },
  };
}

/** Order- and null-insensitive key, so a re-ordered array is not "dirty". */
function canonical(a: Agent): string {
  const c = a.capabilities ?? EMPTY_CAPS;
  const mcp = Object.entries(c.mcp ?? {})
    .map(([k, v]) => [k, [...(v?.tools ?? [])].sort()] as const)
    .filter(([, tools]) => tools.length > 0)
    .sort((x, y) => x[0].localeCompare(y[0]));

  return JSON.stringify([
    a.name.trim(),
    a.slug.trim(),
    a.description,
    a.model,
    a.thinking,
    a.permission_mode,
    a.system_prompt,
    a.claude_config_dir.trim(),
    [...(c.built_in ?? [])].sort(),
    [...(c.local ?? [])].sort(),
    mcp,
  ]);
}

/** The server's slug rule: lowercase letters, digits and hyphens. */
function slugify(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

function permissionLabel(mode: string): string {
  return PERMISSION_LABELS[mode] ?? (mode || "Default");
}

/** Keep a value the server sent but the UI does not enumerate selectable. */
function withCurrent(options: Option[], value: string): Option[] {
  if (options.some((o) => o.value === value)) return options;
  return [
    { value, label: value === "" ? "Unset — server default" : value },
    ...options,
  ];
}
