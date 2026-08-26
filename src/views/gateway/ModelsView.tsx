import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../../lib/api";
import {
  Combobox,
  Dropdown,
  Empty,
  FormRow,
  Search,
  Splitter,
  Switch,
} from "../../components/ui";
import { Icon } from "../../lib/icons";
import { describeError, useResource } from "../../lib/hooks";
import type { ViewId } from "../../lib/nav";
import type {
  GatewayModelAlias,
  GatewayProviderModels,
  GatewayProviderSummary,
  GatewayRouteTarget,
} from "../../lib/types";
import "../../styles/gateway.css";

/* ============================================================================
   LLM Gateway → Models (#427).

   An alias is the entire routing key: whatever a client sends as `model` is
   looked up here, with no prefix parsing anywhere. Each alias carries an
   **ordered** list of targets and an ordered list of fallbacks, and the order
   is the meaning — the first target is preferred, the rest are tried after it
   fails, and the fallbacks are walked once every target has.

   So the editor makes order explicit rather than implying it: targets are
   numbered, moving one is a button, and the list says "tried in order" beside
   itself. A drag handle would look nicer and would tell the user less.

   A target names a provider by its **name**, and the server refuses an alias
   whose target names no configured provider — so the picker offers exactly the
   names that exist rather than a free-text field that 422s.
   ========================================================================== */

type Selection = { kind: "none" } | { kind: "new" } | { kind: "row"; id: string };

/** What is known about one provider's upstream model catalog (#470). */
type Catalog =
  | { state: "loading" }
  | { state: "ready"; models: string[] }
  | { state: "failed"; message: string };

/**
 * Fetch each provider's model catalog **once per view mount**, on demand.
 *
 * Three properties, each of them an acceptance criterion rather than a
 * nicety:
 *
 * - **Never on the save path.** This is driven by rendering a target row, not
 *   by submitting the form, so a slow upstream delays a suggestion list and
 *   nothing else. Saving does not read it at all.
 * - **Once per provider.** `asked` is a ref rather than state precisely because
 *   it must not re-trigger a render — several target rows can name the same
 *   provider, and each of them asks on every keystroke in a sibling field.
 * - **A failure is a value, not a throw.** Every outcome lands in the map, so a
 *   provider that could not be listed is a *known* empty rather than a request
 *   retried forever.
 *
 * Nothing is cached beyond the mount, which is what the issue's "in-memory per
 * session at most" asks for: a key or base URL edited in the Providers tab is
 * picked up the next time this view is opened.
 */
function useProviderCatalogs() {
  const [catalogs, setCatalogs] = useState<Record<string, Catalog>>({});
  const asked = useRef<Set<string>>(new Set());
  // A response arriving after the view closed must not set state on an
  // unmounted tree.
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const request = useCallback((providerId: string) => {
    if (!providerId || asked.current.has(providerId)) return;
    asked.current.add(providerId);
    setCatalogs((c) => ({ ...c, [providerId]: { state: "loading" } }));
    api
      .get<GatewayProviderModels>(
        `/gateway/providers/${encodeURIComponent(providerId)}/models`
      )
      .then((r) => {
        if (!alive.current) return;
        setCatalogs((c) => ({
          ...c,
          [providerId]: { state: "ready", models: r.models ?? [] },
        }));
      })
      .catch((err) => {
        if (!alive.current) return;
        setCatalogs((c) => ({
          ...c,
          [providerId]: { state: "failed", message: describeError(err) },
        }));
      });
  }, []);

  return { catalogs, request };
}

export function GatewayModelsView({
  inspectorOpen,
  onNavigate,
}: {
  inspectorOpen: boolean;
  onNavigate(view: ViewId): void;
}) {
  const aliases = useResource<GatewayModelAlias[] | null>(
    (signal) => api.get("/gateway/models", signal),
    []
  );
  const providers = useResource<GatewayProviderSummary[] | null>(
    (signal) => api.get("/gateway/providers", signal),
    []
  );

  const rows = useMemo(() => aliases.data ?? [], [aliases.data]);
  const providerRows = useMemo(() => providers.data ?? [], [providers.data]);
  const providerNames = useMemo(
    () => providerRows.map((p) => p.name),
    [providerRows]
  );
  const catalogs = useProviderCatalogs();

  const [query, setQuery] = useState("");
  const [selection, setSelection] = useState<Selection>({ kind: "none" });

  useEffect(() => {
    if (aliases.loading) return;
    setSelection((current) => {
      if (current.kind === "new") return current;
      if (current.kind === "row" && rows.some((r) => r.id === current.id)) {
        return current;
      }
      return rows.length > 0 ? { kind: "row", id: rows[0].id } : { kind: "none" };
    });
  }, [rows, aliases.loading]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return rows;
    return rows.filter((r) => r.alias.toLowerCase().includes(q));
  }, [rows, query]);

  const selected =
    selection.kind === "row" ? rows.find((r) => r.id === selection.id) : undefined;

  return (
    <div className="panes">
      <div className="pane-list">
        <div className="toolbar">
          <Search value={query} onChange={setQuery} placeholder="Search aliases" />
          <div className="spacer" />
          <button
            className="iconbtn"
            title="Add an alias"
            disabled={providerNames.length === 0}
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
              <div className="avatar avatar--purple">
                <Icon name="layers" size={14} />
              </div>
              <div className="listrow__body">
                <div className="listrow__top">
                  <span className="listrow__title truncate mono">{r.alias}</span>
                  <span className={`dot ${r.enabled ? "dot--green" : "dot--idle"}`} />
                </div>
                <div className="listrow__preview truncate">
                  {describeRouting(r)}
                </div>
              </div>
            </button>
          ))}
          {/* Not shown when either read failed: "No aliases" and "add a
              provider first" are both claims about stored rows, and a failed
              request knows nothing about them. */}
          {!aliases.loading &&
            !aliases.error &&
            !providers.error &&
            filtered.length === 0 && (
            <Empty
              icon="layers"
              title={rows.length === 0 ? "No aliases" : "No matches"}
              text={
                providerNames.length === 0
                  ? "Add a provider first — an alias has to route somewhere."
                  : rows.length === 0
                    ? "An alias is the model name your tools will ask for."
                    : "Nothing matches that search."
              }
            />
          )}
        </div>
      </div>

      <Splitter variable="--list-w" min={240} max={460} />

      <div className="pane-detail">
        {/* The provider read is surfaced too, and it is the one that would
            otherwise be silent: a failed fetch leaves `providerNames` empty,
            which disables "Add alias" and every provider picker with nothing on
            screen to say why. */}
        {(aliases.error || providers.error) && (
          <div className="scroll gw-errors" style={{ padding: "var(--sp-8)" }}>
            {(
              [
                ["aliases", aliases.error],
                ["providers", providers.error],
              ] as const
            ).map(([source, message]) =>
              message ? (
                // Keyed by which read failed, not by the text — two requests
                // refused for the same reason carry the same message.
                <div className="msgline msgline--error" key={source}>
                  <Icon name="alert" size={13} className="msgline__icon" />
                  <span>{message}</span>
                </div>
              ) : null
            )}
          </div>
        )}
        {!aliases.error && selection.kind === "new" && (
          <AliasForm
            key="new"
            alias={undefined}
            providers={providerRows}
            catalogs={catalogs}
            onDone={(id) => {
              aliases.reload();
              setSelection({ kind: "row", id });
            }}
            onCancel={() => setSelection({ kind: "none" })}
          />
        )}
        {!aliases.error && selected && (
          <AliasForm
            key={selected.id}
            alias={selected}
            providers={providerRows}
            catalogs={catalogs}
            onDone={() => aliases.reload()}
            onDeleted={() => {
              setSelection({ kind: "none" });
              aliases.reload();
            }}
          />
        )}
        {/* Two empty states, and which one shows is the difference between a
            dead end and a next step. With no providers there is nothing an
            alias could route to — `POST /api/gateway/models` refuses a target
            naming no configured provider — so "pick one on the left" is advice
            that cannot be taken, and the list column's own prose says why with
            no way to act on it. The provider read is included in the guard for
            the reason the list column already states: a failed request knows
            nothing about stored rows, so a fetch that errored must never
            produce the "no providers" claim. */}
        {!aliases.error &&
          selection.kind === "none" &&
          !aliases.loading &&
          !providers.loading &&
          (!providers.error && providerNames.length === 0 ? (
            <Empty
              icon="database"
              title="No model provider configured"
              text="An alias is a model name your tools ask for, and it has to route somewhere. Add a provider first."
              action={
                <button
                  className="btn btn--primary"
                  onClick={() => onNavigate("gateway-providers")}
                >
                  Configure a provider
                </button>
              }
            />
          ) : (
            <Empty
              icon="layers"
              title="No alias selected"
              text="Pick one on the left, or define a new model name."
            />
          ))}
      </div>

      {inspectorOpen && (
        <>
          <Splitter variable="--inspector-w" min={220} max={420} invert />
          <aside className="pane-inspector">
            <div className="inspector__head">Aliases</div>
            <div className="inspector__scroll scroll">
              <div className="insp-group">
                <div className="insp-group__title">How dispatch works</div>
                <p className="gw-help">
                  The alias is what a client sends as <code className="mono">model</code>.
                  Targets are tried in order; a retryable failure moves to the
                  next one, and the fallbacks are walked only once every target
                  has failed. A stream that has already sent bytes is never
                  failed over — the answer would be two answers spliced together.
                </p>
              </div>
            </div>
          </aside>
        </>
      )}
    </div>
  );
}

function AliasForm({
  alias,
  providers,
  catalogs,
  onDone,
  onDeleted,
  onCancel,
}: {
  alias: GatewayModelAlias | undefined;
  providers: GatewayProviderSummary[];
  catalogs: ReturnType<typeof useProviderCatalogs>;
  onDone(id: string): void;
  onDeleted?(): void;
  onCancel?(): void;
}) {
  const creating = alias === undefined;

  const [name, setName] = useState(alias?.alias ?? "");
  const [enabled, setEnabled] = useState(alias?.enabled ?? true);
  const [targets, setTargets] = useState<GatewayRouteTarget[]>(
    alias?.routing.targets ?? []
  );
  const [fallbacks, setFallbacks] = useState<GatewayRouteTarget[]>(
    alias?.routing.fallbacks ?? []
  );

  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [notice, setNotice] = useState<string>();
  const [confirmDelete, setConfirmDelete] = useState(false);

  const original = useMemo(
    () =>
      JSON.stringify({
        alias: alias?.alias ?? "",
        enabled: alias?.enabled ?? true,
        targets: alias?.routing.targets ?? [],
        fallbacks: alias?.routing.fallbacks ?? [],
      }),
    [alias]
  );
  const current = JSON.stringify({ alias: name, enabled, targets, fallbacks });
  const changed = creating || current !== original;

  // The server's own rules, applied here so the button explains itself rather
  // than the form 422-ing after a round trip.
  const complete =
    name.trim() !== "" &&
    targets.length > 0 &&
    [...targets, ...fallbacks].every(
      (t) => t.provider !== "" && t.model_id.trim() !== ""
    );
  const canSave = changed && complete;

  /** Back out of an edit in one click, the way every other form here does. */
  function revert() {
    if (!alias) return;
    setName(alias.alias);
    setEnabled(alias.enabled);
    setTargets(alias.routing.targets ?? []);
    setFallbacks(alias.routing.fallbacks ?? []);
    setError(undefined);
  }

  async function save() {
    setBusy(true);
    setError(undefined);
    try {
      const body = {
        alias: name.trim(),
        routing: {
          targets: targets.map(trimTarget),
          fallbacks: fallbacks.map(trimTarget),
        },
        enabled,
      };
      const saved = creating
        ? await api.post<GatewayModelAlias>("/gateway/models", body)
        : await api.put<GatewayModelAlias>(
            `/gateway/models/${encodeURIComponent(alias.id)}`,
            body
          );
      setNotice("Saved.");
      onDone(saved.id);
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    if (!alias) return;
    setBusy(true);
    setError(undefined);
    try {
      await api.del(`/gateway/models/${encodeURIComponent(alias.id)}`);
      onDeleted?.();
    } catch (err) {
      setError(describeError(err));
      setConfirmDelete(false);
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <div className="toolbar">
        <div className="toolbar__title truncate mono">
          {creating ? "New alias" : alias.alias}
        </div>
        {!creating && (
          <span className={`badge ${alias.enabled ? "badge--green" : ""}`}>
            {alias.enabled ? "Enabled" : "Disabled"}
          </span>
        )}
        <div className="spacer" />
        {!creating &&
          (confirmDelete ? (
            <span className="confirm">
              Delete {alias.alias}?
              <button
                className="btn btn--ghost"
                onClick={() => setConfirmDelete(false)}
              >
                Cancel
              </button>
              <button className="btn btn--danger" onClick={remove} disabled={busy}>
                Delete
              </button>
            </span>
          ) : (
            <button
              className="btn btn--danger"
              onClick={() => setConfirmDelete(true)}
            >
              <Icon name="trash" size={13} />
              Delete
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
            <div className="formsec__title">Alias</div>
            <FormRow
              label="Model name"
              help="What a client sends as `model`. This is the whole routing key — there is no prefix parsing."
            >
              <input
                className="field mono"
                value={name}
                placeholder="e.g. fast-sonnet"
                onChange={(e) => setName(e.target.value)}
              />
            </FormRow>
            <FormRow
              label="Enabled"
              help="A disabled alias stays configured and is not routed."
            >
              <Switch on={enabled} onChange={setEnabled} />
            </FormRow>
          </div>

          <div className="divider" />

          <TargetList
            title="Targets"
            caption="Tried in order — the first is preferred, the rest are used when it fails."
            targets={targets}
            providers={providers}
            catalogs={catalogs}
            onChange={setTargets}
          />

          <div className="divider" />

          <TargetList
            title="Fallbacks"
            caption="Walked only after every target above has failed. Optional."
            targets={fallbacks}
            providers={providers}
            catalogs={catalogs}
            onChange={setFallbacks}
          />
        </div>
      </div>

      {(changed || creating) && (
        <div className="savebar">
          <span className="savebar__text">
            {canSave
              ? "You have unsaved changes."
              : name.trim() === ""
                ? "A model name is required."
                : targets.length === 0
                  ? "Add at least one target."
                  : "Every target needs a provider and a model id."}
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

function TargetList({
  title,
  caption,
  targets,
  providers,
  catalogs,
  onChange,
}: {
  title: string;
  caption: string;
  targets: GatewayRouteTarget[];
  providers: GatewayProviderSummary[];
  catalogs: ReturnType<typeof useProviderCatalogs>;
  onChange(next: GatewayRouteTarget[]): void;
}) {
  const providerNames = providers.map((p) => p.name);
  const options = providerNames.map((n) => ({ value: n, label: n }));

  function move(index: number, delta: number) {
    const next = [...targets];
    const to = index + delta;
    if (to < 0 || to >= next.length) return;
    [next[index], next[to]] = [next[to], next[index]];
    onChange(next);
  }

  return (
    <div className="formsec">
      <div className="formsec__title">{title}</div>
      <p className="gw-help">{caption}</p>

      {targets.length === 0 && (
        <p className="gw-help gw-help--muted">Nothing here yet.</p>
      )}

      {targets.map((t, i) => (
        <TargetRow
          key={i}
          target={t}
          providers={providers}
          providerOptions={options}
          catalogs={catalogs}
          first={i === 0}
          last={i === targets.length - 1}
          ordinal={i + 1}
          onChange={(next) =>
            onChange(targets.map((x, j) => (j === i ? next : x)))
          }
          onMove={(delta) => move(i, delta)}
          onRemove={() => onChange(targets.filter((_, j) => j !== i))}
        />
      ))}

      <div className="row">
        <button
          className="btn"
          disabled={providerNames.length === 0}
          onClick={() =>
            onChange([
              ...targets,
              { provider: providerNames[0] ?? "", model_id: "" },
            ])
          }
        >
          <Icon name="plus" size={13} />
          Add {title.toLowerCase().replace(/s$/, "")}
        </button>
      </div>
    </div>
  );
}

/**
 * One routing target: which provider, and which of that provider's models.
 *
 * Its own component because the model field has to *ask* — a row that names a
 * provider requests that provider's catalog on mount and whenever the provider
 * changes, which needs an effect, and an effect cannot live inside a `.map`.
 *
 * **The model id is never constrained to the catalog.** The list is a
 * suggestion: an id typed by hand is saved verbatim, whether or not the fetch
 * succeeded, whether or not it returned anything, and whether or not the typed
 * value appears in it. That is what `Combobox` is for, and it is why the
 * failure path below is a *note* rather than an error — a provider whose list
 * endpoint cannot be reached must leave the row exactly as usable as it was
 * before this feature existed.
 */
function TargetRow({
  target,
  providers,
  providerOptions,
  catalogs,
  first,
  last,
  ordinal,
  onChange,
  onMove,
  onRemove,
}: {
  target: GatewayRouteTarget;
  providers: GatewayProviderSummary[];
  providerOptions: { value: string; label: string }[];
  catalogs: ReturnType<typeof useProviderCatalogs>;
  first: boolean;
  last: boolean;
  ordinal: number;
  onChange(next: GatewayRouteTarget): void;
  onMove(delta: number): void;
  onRemove(): void;
}) {
  // A target names its provider by **name**; the catalog route is keyed by row
  // id. A name with no matching row is possible — an alias stored before a
  // provider was renamed — and simply has no catalog, which is the same
  // degraded state a failed fetch produces.
  const providerId = providers.find((p) => p.name === target.provider)?.id ?? "";
  const { catalogs: known, request } = catalogs;

  useEffect(() => {
    request(providerId);
  }, [providerId, request]);

  const catalog = known[providerId];
  const models = catalog?.state === "ready" ? catalog.models : [];

  // Only ever a *note*, never an error: nothing here can stop a save.
  const note =
    catalog?.state === "failed"
      ? `Could not list this provider's models — type the id. (${catalog.message})`
      : catalog?.state === "ready" && catalog.models.length === 0
        ? "This provider returned no models — type the id."
        : undefined;

  return (
    <div className="gw-target">
      <div className="gw-target__main">
        <span className="gw-target__ord tnum">{ordinal}</span>
        <Dropdown
          value={target.provider}
          options={providerOptions}
          placeholder="Provider"
          onChange={(v) => onChange({ ...target, provider: v })}
          className="gw-target__provider"
        />
        <Combobox
          value={target.model_id}
          options={models}
          ariaLabel="Upstream model id"
          placeholder={
            catalog?.state === "loading" ? "loading models…" : "upstream model id"
          }
          onChange={(v) => onChange({ ...target, model_id: v })}
          className="gw-target__model"
        />
        <button
          className="iconbtn"
          title="Move up"
          disabled={first}
          onClick={() => onMove(-1)}
        >
          <Icon name="arrowUp" size={13} />
        </button>
        <button
          className="iconbtn"
          title="Move down"
          disabled={last}
          onClick={() => onMove(1)}
        >
          <Icon name="arrowDown" size={13} />
        </button>
        <button className="iconbtn" title="Remove" onClick={onRemove}>
          <Icon name="close" size={13} />
        </button>
      </div>
      {note && <p className="gw-target__note truncate" title={note}>{note}</p>}
    </div>
  );
}

function trimTarget(t: GatewayRouteTarget): GatewayRouteTarget {
  return { provider: t.provider, model_id: t.model_id.trim() };
}

function describeRouting(alias: GatewayModelAlias): string {
  const targets = alias.routing.targets ?? [];
  if (targets.length === 0) return "No targets";
  const first = `${targets[0].provider} · ${targets[0].model_id}`;
  const extra =
    targets.length - 1 + (alias.routing.fallbacks ?? []).length;
  return extra > 0 ? `${first} +${extra} more` : first;
}
