import { useEffect, useMemo, useState } from "react";
import { api, ApiError } from "../../lib/api";
import { BrandMark } from "../../components/BrandMark";
import { Dropdown, Empty, FormRow, Search, Splitter, Switch } from "../../components/ui";
import { Icon } from "../../lib/icons";
import { describeError, useResource } from "../../lib/hooks";
import { DESTROY, submitLabel } from "../../lib/formVerbs";
import { SaveBar } from "../../components/SaveBar";
import type {
  GatewayProviderRequest,
  GatewayProviderSummary,
  GatewayProviderType,
  GatewayProviderValidateRequest,
  GatewayProviderValidation,
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
   - **An untouched key field sends no `api_key` at all**, which the server
     reads as "leave the stored one alone" (#426 built `Option<String>` for
     exactly that). It must never send `""`, which *clears* the key: that is
     the whole distance between preserving a secret and destroying one, and it
     is the `PUT /api/integrations/{id}` data-loss bug this surface exists not
     to repeat.

   Until #472 there was a second rule here — a save refused until the key was
   re-typed — described as belt-and-braces over the server's `Option<String>`.
   It was not. The server has always preserved an omitted key, and provider
   dashboards do not show a key twice, so what the rule actually did was block
   every edit to a *timeout* on a provider configured months ago. It is gone,
   and `canSave` requires a typed key only when there is no stored one to fall
   back on.

   What replaced it is a check that answers a different and more useful
   question: **`POST /api/gateway/providers/validate` asks the provider itself**
   whether the credential works, before it is stored. The gate is deliberately
   **soft** — a base that serves no list-models endpoint (a proxy, something
   self-hosted) would otherwise be permanently unsaveable, so "Save anyway" is
   always one click away. A validation gate with no override is a lockout.

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

/**
 * What is known about the credential as currently typed (#472).
 *
 * `undefined` is "not asked yet", which is the state every edit to `type`,
 * `base_url` or the key returns it to — a verdict about fields that have since
 * changed is worse than no verdict, because it reads as one about what is on
 * screen.
 */
type Check =
  | { state: "checking" }
  | { state: "done"; result: GatewayProviderValidation }
  | { state: "failed"; message: string };

/** Which `badge` modifier an outcome wears. */
const OUTCOME_BADGE: Record<GatewayProviderValidation["outcome"], string> = {
  valid: "badge--green",
  unauthorized: "badge--red",
  unreachable: "badge--amber",
  unexpected: "badge--amber",
};

const OUTCOME_LABEL: Record<GatewayProviderValidation["outcome"], string> = {
  valid: "Working",
  unauthorized: "Key refused",
  unreachable: "Unreachable",
  unexpected: "Unexpected",
};

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
              <BrandMark provider={r} />
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
          {/* Not shown when the read failed: "No providers" is a claim about
              the stored rows, and a failed request knows nothing about them.
              The detail pane carries the error. */}
          {!providers.loading && !providers.error && filtered.length === 0 && (
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
  /**
   * Whether the key input is on screen at all. A configured provider shows
   * `••• stored` until the user asks to replace it, so the ordinary edit — a
   * timeout, the enabled flag — never puts an empty secret field in front of
   * someone who has no secret to put in it.
   */
  const [replacingKey, setReplacingKey] = useState(creating || !provider?.has_api_key);

  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [notice, setNotice] = useState<string>();
  const [confirmDelete, setConfirmDelete] = useState(false);

  const [check, setCheck] = useState<Check>();
  /** Set by "Save anyway", and cleared by the same edits a verdict is. */
  const [overridden, setOverridden] = useState(false);

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
  /**
   * A key to check: one just typed, or one already on the server.
   *
   * This is the whole of what #472 relaxes. The server has always read an
   * absent `api_key` as "keep the stored one", so requiring a fresh one was a
   * UI-only rule — and one the user usually could not satisfy, because a
   * provider shows a key once at issuance and never again.
   */
  const haveCredential = hasKey || (!creating && provider.has_api_key);
  const checked = check?.state === "done" && check.result.ok;
  const canSave =
    changed && name.trim() !== "" && haveCredential && (checked || overridden);

  /**
   * A verdict is about the fields it was taken against, so the three inputs it
   * depends on invalidate it. `name`, `enabled` and the timeouts do not: none
   * of them reaches the request.
   */
  function credentialsChanged() {
    setCheck(undefined);
    setOverridden(false);
  }

  /** Back out of an edit in one click, the way every other form here does. */
  function revert() {
    if (!provider) return;
    setName(provider.name);
    setType(provider.type);
    setBaseUrl(provider.base_url);
    setEnabled(provider.enabled);
    setTimeouts(provider.timeouts);
    setApiKey("");
    setReplacingKey(!provider.has_api_key);
    setError(undefined);
    credentialsChanged();
  }

  /**
   * Ask the provider whether this credential works, without spending a token:
   * the route dials the same free list-models endpoint #470 already uses.
   *
   * A failure is a **value**, not a throw — the same doctrine `ModelsView`
   * states for the catalog. Only a malformed request reaches the `catch`, and
   * that one really is an error rather than a verdict.
   */
  async function runCheck() {
    setCheck({ state: "checking" });
    setError(undefined);
    try {
      const body: GatewayProviderValidateRequest = {
        type,
        base_url: baseUrl.trim(),
        // Exactly the same rule the save uses: a key is sent only when typed,
        // and an absent one asks the server to check the stored one.
        ...(creating ? {} : { id: provider.id }),
        ...(hasKey ? { api_key: apiKey.trim() } : {}),
      };
      const result = await api.post<GatewayProviderValidation>(
        "/gateway/providers/validate",
        body
      );
      setCheck({ state: "done", result });
    } catch (err) {
      setCheck({ state: "failed", message: describeError(err) });
    }
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
      setReplacingKey(false);
      setNotice("Saved.");
      credentialsChanged();
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
          ? `A model alias still routes to ${provider.name}. Delete or re-point it first.`
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
              {DESTROY} {provider.name}? Its stored key goes with it.
              <button
                className="btn btn--ghost"
                onClick={() => setConfirmDelete(false)}
              >
                Cancel
              </button>
              <button className="btn btn--danger" onClick={remove} disabled={busy}>
                {DESTROY}
              </button>
            </span>
          ) : (
            <button
              className="btn btn--danger"
              onClick={() => setConfirmDelete(true)}
            >
              <Icon name="trash" size={13} />
              {DESTROY}
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
                onChange={(v) => {
                  setType(v as GatewayProviderType);
                  credentialsChanged();
                }}
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
                onChange={(e) => {
                  setBaseUrl(e.target.value);
                  credentialsChanged();
                }}
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
                    ? "A key is stored. Agento cannot show it back to you, and does not need to — leave this alone and the stored one is kept."
                    : "No key is stored for this provider — it cannot serve a request until you add one."}
              </span>
            </div>

            <FormRow
              label="API key"
              help={
                replacingKey
                  ? "Sent only when you type one, and never returned by any read."
                  : "Untouched, so the save sends no key at all and the stored one survives."
              }
            >
              {replacingKey ? (
                <input
                  className="field mono"
                  type="password"
                  autoComplete="off"
                  value={apiKey}
                  placeholder="sk-…"
                  onChange={(e) => {
                    setApiKey(e.target.value);
                    credentialsChanged();
                  }}
                />
              ) : (
                <div className="gw-storedkey">
                  <span className="mono">••••••••••• stored</span>
                  <button
                    className="btn btn--ghost"
                    onClick={() => {
                      setReplacingKey(true);
                      credentialsChanged();
                    }}
                  >
                    Replace key
                  </button>
                </div>
              )}
            </FormRow>

            <FormRow
              label="Check"
              help={
                haveCredential
                  ? "Asks the provider to list its models — it authenticates the same credential a request would, and spends nothing."
                  : "Enter an API key first."
              }
            >
              <button
                className="btn"
                style={{ alignSelf: "flex-start" }}
                onClick={runCheck}
                disabled={!haveCredential || check?.state === "checking" || busy}
              >
                <Icon name="check" size={13} />
                {check?.state === "checking"
                  ? "Checking…"
                  : "Check these credentials"}
              </button>

              {check?.state === "done" && (
                <>
                  <div className="gw-storedkey">
                    <span className={`badge ${OUTCOME_BADGE[check.result.outcome]}`}>
                      {OUTCOME_LABEL[check.result.outcome]}
                    </span>
                    {check.result.status !== undefined && (
                      <span className="gw-help" style={{ margin: 0 }}>
                        HTTP {check.result.status}
                      </span>
                    )}
                  </div>
                  <div
                    className={`msgline ${check.result.ok ? "msgline--ok" : "msgline--error"}`}
                  >
                    <span>
                      {check.result.message}
                      {check.result.ok && check.result.models.length > 0 && (
                        <>
                          {" "}
                          It serves {check.result.models.length} model
                          {check.result.models.length === 1 ? "" : "s"}, including{" "}
                          <span className="mono">
                            {check.result.models.slice(0, 3).join(", ")}
                          </span>
                          .
                        </>
                      )}
                    </span>
                  </div>
                </>
              )}

              {check?.state === "failed" && (
                <div className="msgline msgline--error">
                  <span>{check.message}</span>
                </div>
              )}
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
        /* `onCancel` is passed only when `provider` is undefined, so it is
           present exactly while `creating` — and abandoning a draft is what
           `Discard` names, where `Revert` restores a stored row. See
           `lib/formVerbs.ts`; the word itself comes from `creating`, so only
           the handler differs. */
        <SaveBar
          creating={creating}
          busy={busy}
          canSubmit={canSave}
          message={
            canSave
              ? "You have unsaved changes."
              : name.trim() === ""
                ? "A name is required."
                : !haveCredential
                  ? "Enter the API key to save."
                  : "Check the credentials, or save anyway."
          }
          onDiscard={onCancel ?? revert}
          onSubmit={save}
          extra={
            /*
              The override, and it is not optional. A base that serves no
              list-models endpoint — a proxy, something self-hosted, an
              OpenAI-compatible vendor that implements only completions —
              cannot ever produce a green verdict, so a gate without this would
              make it permanently unsaveable. It is offered whenever the check
              has not passed, including before it has been run at all: a user
              who knows their setup should not have to fail a check first.

              It **saves**, rather than merely arming Save. Setting `overridden`
              alone would make the button vanish on click — the condition here
              is what renders it — leaving the user hunting for the Save it just
              enabled, from a label that promised the save itself. `save()`
              reads neither `overridden` nor `canSave`, so calling it here is
              the whole of it; the flag is still set, so a failed save leaves
              Save armed.

              It is the sole reason `SaveBar` takes an `extra` slot: every other
              savebar in the app is exactly two buttons.
            */
            !checked && !overridden && haveCredential && name.trim() !== "" ? (
              <button
                className="btn"
                onClick={() => {
                  setOverridden(true);
                  void save();
                }}
                disabled={busy}
              >
                {`${submitLabel(creating, false)} anyway`}
              </button>
            ) : undefined
          }
        />
      )}
    </>
  );
}

function labelForType(type: GatewayProviderType): string {
  return TYPES.find((t) => t.value === type)?.label ?? type;
}
