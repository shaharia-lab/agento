import { useEffect, useMemo, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { Dropdown, Empty, FormRow, Search, Splitter, Switch } from "../../components/ui";
import { Icon } from "../../lib/icons";
import { describeError, useResource } from "../../lib/hooks";
import type {
  GatewayProviderRequest,
  GatewayProviderSummary,
  GatewayProviderType,
  GatewayTimeouts,
} from "../../lib/types";
import "../../styles/gateway.css";

/* ============================================================================
   LLM Gateway → Providers (#427).

   The upstreams the gateway can dispatch to, and the one place in this section
   that holds a secret.

   Two rules, both load-bearing:

   - **The API key is write-only.** No read answers one — `GET
     /api/gateway/providers` returns `has_api_key`, a boolean computed in SQL —
     so there is nothing to render, and the input starts empty on every load.
     A masked value would be a lie about what this app knows.
   - **A save with pending changes refuses until the key is re-entered.** The
     server preserves a stored key when the field is *absent* (#426 built
     `Option<String>` for exactly that), so this is the second half of a
     belt-and-braces pair rather than the only guard: it is what stops the
     integrations bug — a scrubbed read written straight back wiping the
     secret — from ever depending on one side alone.

   Deleting is refused by the server while an alias still routes to the
   provider, which is checked in code because the reference lives inside a JSON
   column and no foreign key can hold it. That 409's own sentence reads oddly
   for a delete, so this view writes its own.
   ========================================================================== */

const TYPES: { value: GatewayProviderType; label: string }[] = [
  { value: "anthropic", label: "Anthropic" },
  { value: "openai", label: "OpenAI" },
  { value: "gemini", label: "Google Gemini" },
  { value: "glm", label: "Z.AI GLM" },
];

/** Matches `ferrox_providers`' own defaults, which the server applies too. */
const DEFAULT_TIMEOUTS: GatewayTimeouts = {
  connect_secs: 10,
  ttfb_secs: 60,
  idle_secs: 30,
};

/** What an empty base URL means, per type — shown as the input's placeholder. */
const BASE_URL_HINT: Record<GatewayProviderType, string> = {
  anthropic: "https://api.anthropic.com (default)",
  openai: "https://api.openai.com/v1 (default)",
  gemini: "the Gemini default endpoint",
  glm: "https://api.z.ai/api/paas/v4 (required for GLM)",
};

type Selection =
  | { kind: "none" }
  | { kind: "new" }
  | { kind: "row"; id: string };

export function GatewayProvidersView({ inspectorOpen }: { inspectorOpen: boolean }) {
  const providers = useResource<GatewayProviderSummary[] | null>(
    (signal) => api.get("/gateway/providers", signal),
    []
  );
  const rows = useMemo(() => providers.data ?? [], [providers.data]);

  const [query, setQuery] = useState("");
  const [selection, setSelection] = useState<Selection>({ kind: "none" });

  // Never leave the selection pointing at a row that is gone, and select the
  // first one once the list has actually loaded.
  useEffect(() => {
    if (providers.loading) return;
    setSelection((current) => {
      if (current.kind === "new") return current;
      if (current.kind === "row" && rows.some((r) => r.id === current.id)) {
        return current;
      }
      return rows.length > 0 ? { kind: "row", id: rows[0].id } : { kind: "none" };
    });
  }, [rows, providers.loading]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return rows;
    return rows.filter(
      (r) => r.name.toLowerCase().includes(q) || r.type.includes(q)
    );
  }, [rows, query]);

  const selected =
    selection.kind === "row"
      ? rows.find((r) => r.id === selection.id)
      : undefined;

  return (
    <div className="panes">
      <div className="pane-list">
        <div className="toolbar">
          <Search value={query} onChange={setQuery} placeholder="Search providers" />
          <div className="spacer" />
          <button
            className="iconbtn"
            title="Add a provider"
            onClick={() => setSelection({ kind: "new" })}
          >
            <Icon name="plus" size={14} />
          </button>
        </div>
        <div className="list__scroll scroll">
          {filtered.map((r) => (
            <button
              key={r.id}
              className={`listrow ${
                selection.kind === "row" && selection.id === r.id
                  ? "listrow--active"
                  : ""
              }`}
              onClick={() => setSelection({ kind: "row", id: r.id })}
            >
              <div className="avatar avatar--accent">
                <Icon name="database" size={14} />
              </div>
              <div className="listrow__body">
                <div className="listrow__top">
                  <span className="listrow__title truncate">{r.name}</span>
                  <span className={`dot ${r.enabled ? "dot--green" : "dot--idle"}`} />
                </div>
                <div className="listrow__preview truncate">
                  {labelForType(r.type)}
                  {r.has_api_key ? "" : " · no API key"}
                </div>
              </div>
            </button>
          ))}
          {!providers.loading && filtered.length === 0 && (
            <Empty
              icon="database"
              title={rows.length === 0 ? "No providers" : "No matches"}
              text={
                rows.length === 0
                  ? "Add the upstream you want the gateway to dispatch to."
                  : "Nothing matches that search."
              }
            />
          )}
        </div>
      </div>

      <Splitter variable="--list-w" min={240} max={460} />

      <div className="pane-detail">
        {providers.error && (
          <div className="scroll" style={{ padding: "var(--sp-8)" }}>
            <div className="msgline msgline--error">
              <Icon name="alert" size={13} className="msgline__icon" />
              <span>{providers.error}</span>
            </div>
          </div>
        )}
        {!providers.error && selection.kind === "new" && (
          <ProviderForm
            key="new"
            provider={undefined}
            onDone={(id) => {
              providers.reload();
              setSelection({ kind: "row", id });
            }}
            onCancel={() => setSelection({ kind: "none" })}
          />
        )}
        {!providers.error && selected && (
          <ProviderForm
            key={selected.id}
            provider={selected}
            onDone={() => providers.reload()}
            onDeleted={() => {
              setSelection({ kind: "none" });
              providers.reload();
            }}
          />
        )}
        {!providers.error &&
          selection.kind === "none" &&
          !providers.loading && (
            <Empty
              icon="database"
              title="No provider selected"
              text="Pick one on the left, or add a new upstream."
            />
          )}
      </div>

      {inspectorOpen && (
        <>
          <Splitter variable="--inspector-w" min={220} max={420} invert />
          <aside className="pane-inspector">
            <div className="inspector__head">Providers</div>
            <div className="inspector__scroll scroll">
              <div className="insp-group">
                <div className="insp-group__title">How routing finds these</div>
                <p className="gw-help">
                  A model alias names a provider by its <strong>name</strong>,
                  not its id, and nothing in SQL enforces that link. Renaming a
                  provider that an alias routes to will break the alias, and
                  deleting one is refused while any alias still names it.
                </p>
              </div>
            </div>
          </aside>
        </>
      )}
    </div>
  );
}

function ProviderForm({
  provider,
  onDone,
  onDeleted,
  onCancel,
}: {
  provider: GatewayProviderSummary | undefined;
  onDone(id: string): void;
  onDeleted?(): void;
  onCancel?(): void;
}) {
  const creating = provider === undefined;

  const [name, setName] = useState(provider?.name ?? "");
  const [type, setType] = useState<GatewayProviderType>(provider?.type ?? "openai");
  const [baseUrl, setBaseUrl] = useState(provider?.base_url ?? "");
  const [enabled, setEnabled] = useState(provider?.enabled ?? true);
  const [timeouts, setTimeouts] = useState<GatewayTimeouts>(
    provider?.timeouts ?? DEFAULT_TIMEOUTS
  );
  // Write-only: it starts empty on every load and no response can fill it.
  const [apiKey, setApiKey] = useState("");

  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [notice, setNotice] = useState<string>();
  const [confirmDelete, setConfirmDelete] = useState(false);

  const changed =
    creating ||
    name !== provider.name ||
    type !== provider.type ||
    baseUrl !== provider.base_url ||
    enabled !== provider.enabled ||
    timeouts.connect_secs !== provider.timeouts.connect_secs ||
    timeouts.ttfb_secs !== provider.timeouts.ttfb_secs ||
    timeouts.idle_secs !== provider.timeouts.idle_secs ||
    apiKey.trim() !== "";

  const hasKey = apiKey.trim() !== "";
  const canSave = changed && name.trim() !== "" && hasKey;

  /** Back out of an edit in one click, the way every other form here does. */
  function revert() {
    if (!provider) return;
    setName(provider.name);
    setType(provider.type);
    setBaseUrl(provider.base_url);
    setEnabled(provider.enabled);
    setTimeouts(provider.timeouts);
    setApiKey("");
    setError(undefined);
  }

  async function save() {
    setBusy(true);
    setError(undefined);
    try {
      const body: GatewayProviderRequest = {
        name: name.trim(),
        type,
        base_url: baseUrl.trim(),
        timeouts,
        enabled,
        // Sent only when the user typed one. The server reads an absent key as
        // "keep the stored one", so this field never destroys a secret.
        ...(hasKey ? { api_key: apiKey.trim() } : {}),
      };
      const saved = creating
        ? await api.post<GatewayProviderSummary>("/gateway/providers", body)
        : await api.put<GatewayProviderSummary>(
            `/gateway/providers/${encodeURIComponent(provider.id)}`,
            body
          );
      setApiKey("");
      setNotice("Saved.");
      onDone(saved.id);
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    if (!provider) return;
    setBusy(true);
    setError(undefined);
    try {
      await api.del(`/gateway/providers/${encodeURIComponent(provider.id)}`);
      onDeleted?.();
    } catch (err) {
      // The server's 409 is phrased for a create ("… already exists"), which
      // reads as nonsense on a delete. The status is what carries the meaning.
      setError(
        err instanceof ApiError && err.status === 409
          ? `A model alias still routes to ${provider.name}. Remove or re-point it first.`
          : describeError(err)
      );
      setConfirmDelete(false);
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <div className="toolbar">
        <div className="toolbar__title truncate">
          {creating ? "New provider" : provider.name}
        </div>
        {!creating && (
          <span className={`badge ${provider.enabled ? "badge--green" : ""}`}>
            {provider.enabled ? "Enabled" : "Disabled"}
          </span>
        )}
        <div className="spacer" />
        {!creating &&
          (confirmDelete ? (
            <span className="confirm">
              Remove {provider.name}?
              <button
                className="btn btn--ghost"
                onClick={() => setConfirmDelete(false)}
              >
                Cancel
              </button>
              <button className="btn btn--danger" onClick={remove} disabled={busy}>
                Remove
              </button>
            </span>
          ) : (
            <button
              className="btn btn--danger"
              onClick={() => setConfirmDelete(true)}
            >
              <Icon name="trash" size={13} />
              Remove
            </button>
          ))}
      </div>

      <div className="scroll" style={{ flex: 1, padding: "var(--sp-8)" }}>
        <div className="form">
          {error && (
            <div className="msgline msgline--error">
              <Icon name="alert" size={13} className="msgline__icon" />
              <span>{error}</span>
            </div>
          )}
          {notice && !changed && (
            <div className="msgline msgline--ok">
              <Icon name="check" size={13} className="msgline__icon" />
              <span>{notice}</span>
            </div>
          )}

          <div className="formsec">
            <div className="formsec__title">Upstream</div>

            <FormRow
              label="Name"
              help="Model aliases route to this name, so changing it breaks any alias that uses it."
            >
              <input
                className="field"
                value={name}
                placeholder="e.g. openai-main"
                onChange={(e) => setName(e.target.value)}
              />
            </FormRow>

            <FormRow label="Type" help="Which adapter serves this provider.">
              <Dropdown
                value={type}
                options={TYPES}
                onChange={(v) => setType(v as GatewayProviderType)}
              />
            </FormRow>

            <FormRow
              label="Base URL"
              help="Leave empty to use the adapter's own endpoint."
            >
              <input
                className="field"
                value={baseUrl}
                placeholder={BASE_URL_HINT[type]}
                onChange={(e) => setBaseUrl(e.target.value)}
              />
            </FormRow>

            <FormRow
              label="Enabled"
              help="A disabled provider stays configured and is not dispatched to."
            >
              <Switch on={enabled} onChange={setEnabled} />
            </FormRow>
          </div>

          <div className="divider" />

          <div className="formsec">
            <div className="formsec__title">Credentials</div>

            <div className="msgline msgline--warn">
              <Icon name="shield" size={13} className="msgline__icon" />
              <span>
                {creating
                  ? "The API key is stored locally and is never shown again."
                  : provider.has_api_key
                    ? "A key is stored. Agento cannot show it back to you, so any save has to be given one."
                    : "No key is stored for this provider — it cannot serve a request until you add one."}
              </span>
            </div>

            <FormRow
              label="API key"
              help="Sent only when you type one, and never returned by any read."
            >
              <input
                className="field mono"
                type="password"
                autoComplete="off"
                value={apiKey}
                placeholder="sk-…"
                onChange={(e) => setApiKey(e.target.value)}
              />
            </FormRow>
          </div>

          <div className="divider" />

          <div className="formsec">
            <div className="formsec__title">Timeouts</div>
            {(
              [
                ["connect_secs", "Connect", "Seconds to establish the connection."],
                ["ttfb_secs", "First byte", "Seconds to wait for the first token."],
                ["idle_secs", "Idle", "Seconds between tokens before giving up."],
              ] as const
            ).map(([key, label, help]) => (
              <FormRow key={key} label={label} help={help}>
                <input
                  className="field field--sm tnum"
                  type="number"
                  min={1}
                  value={timeouts[key]}
                  onChange={(e) =>
                    setTimeouts((t) => ({
                      ...t,
                      [key]: Math.max(1, Number(e.target.value) || 1),
                    }))
                  }
                />
              </FormRow>
            ))}
          </div>
        </div>
      </div>

      {(changed || creating) && (
        <div className="savebar">
          <span className="savebar__text">
            {canSave
              ? "You have unsaved changes."
              : name.trim() === ""
                ? "A name is required."
                : "Enter the API key to save."}
          </span>
          {onCancel ? (
            <button className="btn" onClick={onCancel} disabled={busy}>
              Cancel
            </button>
          ) : (
            <button className="btn" onClick={revert} disabled={busy}>
              Revert
            </button>
          )}
          <button
            className="btn btn--primary"
            onClick={save}
            disabled={!canSave || busy}
          >
            {busy ? "Saving…" : "Save"}
          </button>
        </div>
      )}
    </>
  );
}

function labelForType(type: GatewayProviderType): string {
  return TYPES.find((t) => t.value === type)?.label ?? type;
}
