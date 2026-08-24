import { useEffect, useMemo, useState } from "react";
import { api } from "../../lib/api";
import { Dropdown, Empty, FormRow, Search, Splitter, Switch } from "../../components/ui";
import { Icon } from "../../lib/icons";
import { describeError, useResource } from "../../lib/hooks";
import type {
  GatewayModelAlias,
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

export function GatewayModelsView({ inspectorOpen }: { inspectorOpen: boolean }) {
  const aliases = useResource<GatewayModelAlias[] | null>(
    (signal) => api.get("/gateway/models", signal),
    []
  );
  const providers = useResource<GatewayProviderSummary[] | null>(
    (signal) => api.get("/gateway/providers", signal),
    []
  );

  const rows = useMemo(() => aliases.data ?? [], [aliases.data]);
  const providerNames = useMemo(
    () => (providers.data ?? []).map((p) => p.name),
    [providers.data]
  );

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
          {!aliases.loading && filtered.length === 0 && (
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
          <div className="scroll" style={{ padding: "var(--sp-8)" }}>
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
            providerNames={providerNames}
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
            providerNames={providerNames}
            onDone={() => aliases.reload()}
            onDeleted={() => {
              setSelection({ kind: "none" });
              aliases.reload();
            }}
          />
        )}
        {!aliases.error && selection.kind === "none" && !aliases.loading && (
          <Empty
            icon="layers"
            title="No alias selected"
            text="Pick one on the left, or define a new model name."
          />
        )}
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
  providerNames,
  onDone,
  onDeleted,
  onCancel,
}: {
  alias: GatewayModelAlias | undefined;
  providerNames: string[];
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
            providerNames={providerNames}
            onChange={setTargets}
          />

          <div className="divider" />

          <TargetList
            title="Fallbacks"
            caption="Walked only after every target above has failed. Optional."
            targets={fallbacks}
            providerNames={providerNames}
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
  providerNames,
  onChange,
}: {
  title: string;
  caption: string;
  targets: GatewayRouteTarget[];
  providerNames: string[];
  onChange(next: GatewayRouteTarget[]): void;
}) {
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
        <div className="gw-target" key={i}>
          <span className="gw-target__ord tnum">{i + 1}</span>
          <Dropdown
            value={t.provider}
            options={options}
            placeholder="Provider"
            onChange={(v) =>
              onChange(targets.map((x, j) => (j === i ? { ...x, provider: v } : x)))
            }
            className="gw-target__provider"
          />
          <input
            className="field mono gw-target__model"
            value={t.model_id}
            placeholder="upstream model id"
            onChange={(e) =>
              onChange(
                targets.map((x, j) =>
                  j === i ? { ...x, model_id: e.target.value } : x
                )
              )
            }
          />
          <button
            className="iconbtn"
            title="Move up"
            disabled={i === 0}
            onClick={() => move(i, -1)}
          >
            <Icon name="arrowUp" size={13} />
          </button>
          <button
            className="iconbtn"
            title="Move down"
            disabled={i === targets.length - 1}
            onClick={() => move(i, 1)}
          >
            <Icon name="arrowDown" size={13} />
          </button>
          <button
            className="iconbtn"
            title="Remove"
            onClick={() => onChange(targets.filter((_, j) => j !== i))}
          >
            <Icon name="close" size={13} />
          </button>
        </div>
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
