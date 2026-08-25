import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../lib/api";
import { describeError, useResource } from "../lib/hooks";
import { dateTime, relativeTime } from "../lib/format";
import { Icon } from "../lib/icons";
import { openExternal } from "../lib/tauri";
import {
  Checkbox,
  Dropdown,
  Empty,
  InspGroup,
  InspRow,
  Search,
  Segmented,
  Splitter,
  Switch,
} from "../components/ui";
import type {
  Agent,
  AvailableTool,
  Integration,
  ServiceConfig,
  TriggerRule,
  WebhookStatus,
} from "../lib/types";
import {
  PROVIDERS,
  providerFor,
  unavailableCopy,
  type AuthMode,
  type Provider,
} from "./integrations/catalog";
import "../styles/integrations.css";

/* ============================================================================
   Integrations — connect a provider, then choose what its agents may call.
   ========================================================================== */

type Selection =
  | { kind: "integration"; id: string }
  | { kind: "provider"; type: string };

type Services = Record<string, ServiceConfig>;

function emptyServices(provider: Provider, on: boolean): Services {
  const out: Services = {};
  for (const s of provider.services) {
    out[s.key] = { enabled: on, tools: on ? s.tools.map((t) => t.name) : [] };
  }
  return out;
}

function buildCredentials(
  provider: Provider,
  mode: AuthMode,
  values: Record<string, string>
): Record<string, string> {
  const out: Record<string, string> = {};
  if (provider.hasAuthModeField) out.auth_mode = mode.value;
  for (const f of mode.fields) out[f.key] = (values[f.key] ?? "").trim();
  return out;
}

function credentialsComplete(mode: AuthMode, values: Record<string, string>): boolean {
  return mode.fields.every((f) => (values[f.key] ?? "").trim() !== "");
}

function countTools(services: Services | null | undefined): number {
  if (!services) return 0;
  return Object.values(services).reduce(
    (n, s) => n + (s.enabled ? (s.tools?.length ?? 0) : 0),
    0
  );
}

/* ============================================================================
   View
   ========================================================================== */

export function IntegrationsView({ inspectorOpen }: { inspectorOpen: boolean }) {
  const [query, setQuery] = useState("");
  const [selection, setSelection] = useState<Selection>();

  const integrations = useResource(
    (signal) => api.get<Integration[] | null>("/integrations", signal),
    []
  );
  const tools = useResource(
    (signal) => api.get<AvailableTool[] | null>("/integrations/available-tools", signal),
    []
  );

  const list = useMemo(() => integrations.data ?? [], [integrations.data]);

  const reloadAll = useCallback(() => {
    integrations.reload();
    tools.reload();
  }, [integrations, tools]);

  // Land on something as soon as the list arrives, and never leave the
  // selection pointing at an integration that has been removed.
  useEffect(() => {
    if (integrations.loading) return;
    setSelection((current) => {
      if (current?.kind === "integration" && list.some((i) => i.id === current.id)) {
        return current;
      }
      if (current?.kind === "provider") return current;
      if (list.length > 0) return { kind: "integration", id: list[0].id };
      return { kind: "provider", type: PROVIDERS[0].type };
    });
  }, [integrations.loading, list]);

  const needle = query.trim().toLowerCase();
  const connected = list.filter(
    (i) =>
      !needle ||
      i.name.toLowerCase().includes(needle) ||
      i.type.toLowerCase().includes(needle)
  );
  const catalog = PROVIDERS.filter(
    (p) =>
      !needle ||
      p.label.toLowerCase().includes(needle) ||
      p.blurb.toLowerCase().includes(needle)
  );

  const selected =
    selection?.kind === "integration"
      ? list.find((i) => i.id === selection.id)
      : undefined;
  const selectedProvider =
    selection?.kind === "provider"
      ? providerFor(selection.type)
      : selected
        ? providerFor(selected.type)
        : undefined;

  return (
    <div className="panes">
      <div className="pane-list">
        <div className="toolbar">
          <Search value={query} onChange={setQuery} placeholder="Search integrations" />
        </div>
        <div className="list__scroll scroll">
          <div className="listgroup">Connected</div>
          {connected.length === 0 && (
            <div
              style={{
                padding: "var(--sp-4) var(--sp-6)",
                fontSize: "var(--text-sm)",
                color: "var(--fg-tertiary)",
              }}
            >
              {integrations.loading ? "Loading…" : "Nothing connected yet."}
            </div>
          )}
          {connected.map((i) => (
            <IntegrationRow
              key={i.id}
              item={i}
              active={selection?.kind === "integration" && selection.id === i.id}
              onSelect={() => setSelection({ kind: "integration", id: i.id })}
            />
          ))}

          <div className="listgroup">Add integration</div>
          {catalog.map((p) => (
            <ProviderRow
              key={p.type}
              provider={p}
              active={selection?.kind === "provider" && selection.type === p.type}
              onSelect={() => setSelection({ kind: "provider", type: p.type })}
            />
          ))}
        </div>
      </div>

      <Splitter variable="--list-w" min={240} max={460} />

      <div className="pane-detail">
        {integrations.error && !integrations.data ? (
          <Empty
            icon="alert"
            title="Integrations unavailable"
            text={integrations.error}
            action={
              <button className="btn btn--lg" onClick={integrations.reload}>
                <Icon name="refresh" size={14} />
                Try again
              </button>
            }
          />
        ) : selection?.kind === "provider" && selectedProvider ? (
          <ConnectForm
            key={selectedProvider.type}
            provider={selectedProvider}
            onCreated={(id) => {
              reloadAll();
              setSelection({ kind: "integration", id });
            }}
          />
        ) : selected && selectedProvider ? (
          <IntegrationDetail
            key={selected.id}
            item={selected}
            provider={selectedProvider}
            onChanged={reloadAll}
            onDeleted={() => {
              setSelection({ kind: "provider", type: selected.type });
              reloadAll();
            }}
          />
        ) : selected ? (
          <Empty icon="plug" {...unavailableCopy(selected.type)} />
        ) : (
          <Empty icon="plug" title="Integrations" text="Choose an integration or a provider." />
        )}
      </div>

      {inspectorOpen && (
        <>
          <Splitter variable="--inspector-w" min={220} max={420} invert />
          <aside className="pane-inspector">
            <div className="inspector__head">Integration</div>
            <div className="inspector__scroll scroll">
              {selected ? (
                <ConnectedInspector
                  item={selected}
                  provider={selectedProvider}
                  tools={tools.data ?? []}
                />
              ) : selectedProvider ? (
                <ProviderInspector provider={selectedProvider} />
              ) : null}
            </div>
          </aside>
        </>
      )}
    </div>
  );
}

/* --- List rows ----------------------------------------------------------- */

function IntegrationRow({
  item,
  active,
  onSelect,
}: {
  item: Integration;
  active: boolean;
  onSelect(): void;
}) {
  const provider = providerFor(item.type);
  return (
    <button className={`listrow ${active ? "listrow--active" : ""}`} onClick={onSelect}>
      <div className={`avatar avatar--${provider?.tone ?? "accent"}`}>
        <Icon name={provider?.icon ?? "plug"} size={15} />
      </div>
      <div className="listrow__body">
        <div className="listrow__top">
          <span className="listrow__title">{item.name}</span>
          {item.authenticated && item.enabled && <span className="dot dot--green" />}
          {item.authenticated && !item.enabled && <span className="dot dot--idle" />}
          {!item.authenticated && <span className="dot dot--amber" />}
        </div>
        <div className="listrow__preview">
          {provider?.label ?? item.type} ·{" "}
          {item.authenticated ? `${countTools(item.services)} tools` : "Not connected"}
        </div>
      </div>
    </button>
  );
}

function ProviderRow({
  provider,
  active,
  onSelect,
}: {
  provider: Provider;
  active: boolean;
  onSelect(): void;
}) {
  return (
    <button className={`listrow ${active ? "listrow--active" : ""}`} onClick={onSelect}>
      <div className={`avatar avatar--${provider.tone}`}>
        <Icon name={provider.icon} size={15} />
      </div>
      <div className="listrow__body">
        <div className="listrow__top">
          <span className="listrow__title">{provider.label}</span>
        </div>
        <div className="listrow__preview">{provider.blurb}</div>
      </div>
    </button>
  );
}

/* --- Connect a new integration ------------------------------------------- */

function ConnectForm({
  provider,
  onCreated,
}: {
  provider: Provider;
  onCreated(id: string): void;
}) {
  const [name, setName] = useState(provider.label);
  const [modeValue, setModeValue] = useState(provider.modes[0].value);
  const [values, setValues] = useState<Record<string, string>>({});
  const [services, setServices] = useState<Services>(() => emptyServices(provider, true));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  const mode = provider.modes.find((m) => m.value === modeValue) ?? provider.modes[0];
  const ready = name.trim() !== "" && credentialsComplete(mode, values);

  async function create() {
    setBusy(true);
    setError(undefined);
    try {
      const created = await api.post<Integration>("/integrations", {
        name: name.trim(),
        type: provider.type,
        enabled: true,
        credentials: buildCredentials(provider, mode, values),
        services,
      });
      onCreated(created.id);
    } catch (err) {
      setError(describeError(err));
      setBusy(false);
    }
  }

  return (
    <>
      <div className="toolbar">
        <div className={`avatar avatar--${provider.tone}`} style={{ width: 22, height: 22 }}>
          <Icon name={provider.icon} size={13} />
        </div>
        <div className="toolbar__title">Connect {provider.label}</div>
        <div className="spacer" />
        <button className="btn btn--primary" onClick={create} disabled={!ready || busy}>
          <Icon name="plus" size={13} />
          {busy ? "Creating…" : "Create"}
        </button>
      </div>

      <div className="scroll" style={{ flex: 1, padding: "var(--sp-8)" }}>
        <div className="form">
          <div className="formsec">
            <div className="formsec__title">About</div>
            <p
              style={{
                fontSize: "var(--text-md)",
                lineHeight: "var(--leading-relaxed)",
                color: "var(--fg-secondary)",
                maxWidth: 560,
              }}
            >
              {provider.blurb}. Once connected, any agent you grant access to can call
              these tools on your behalf.
            </p>
          </div>

          <div className="divider" />

          <div className="formsec">
            <div className="formsec__title">Connection</div>
            <div className="formrow">
              <div className="formrow__label">Name</div>
              <div className="formrow__control">
                <label className="field">
                  <input
                    value={name}
                    onChange={(e) => setName(e.target.value)}
                    placeholder={provider.label}
                    spellCheck={false}
                  />
                </label>
                <div className="formrow__help">
                  How this connection is labelled in Agento. Useful when you connect the
                  same provider twice.
                </div>
              </div>
            </div>

            {provider.modes.length > 1 && (
              <div className="formrow">
                <div className="formrow__label">Auth method</div>
                <div className="formrow__control">
                  <Segmented
                    value={modeValue}
                    options={provider.modes.map((m) => ({ value: m.value, label: m.label }))}
                    onChange={setModeValue}
                  />
                </div>
              </div>
            )}

            <CredentialFields mode={mode} values={values} onChange={setValues} />

            {mode.kind === "oauth" && (
              <div className="msgline">
                <span className="msgline__icon">
                  <Icon name="info" size={13} />
                </span>
                <span>
                  After the integration is created you will be sent to {provider.label} in
                  your browser to authorise it.
                </span>
              </div>
            )}
          </div>

          <div className="divider" />

          <div className="formsec">
            <div className="formsec__title">Services and tools</div>
            <div className="formrow__help">
              Only the tools you leave on are exposed to agents. You can change this later.
            </div>
            <ServiceEditor provider={provider} services={services} onChange={setServices} />
          </div>

          {error && (
            <div className="msgline msgline--error">
              <span className="msgline__icon">
                <Icon name="alert" size={13} />
              </span>
              <span>{error}</span>
            </div>
          )}
        </div>
      </div>
    </>
  );
}

/* --- Credentials --------------------------------------------------------- */

function CredentialFields({
  mode,
  values,
  onChange,
}: {
  mode: AuthMode;
  values: Record<string, string>;
  onChange(next: Record<string, string>): void;
}) {
  return (
    <>
      {mode.fields.map((f) => (
        <div className="formrow" key={f.key}>
          <div className="formrow__label">{f.label}</div>
          <div className="formrow__control">
            <label className="field">
              <input
                type={f.secret ? "password" : "text"}
                value={values[f.key] ?? ""}
                onChange={(e) => onChange({ ...values, [f.key]: e.target.value })}
                placeholder={f.placeholder}
                spellCheck={false}
                autoComplete="off"
              />
            </label>
            {f.help && <div className="formrow__help">{f.help}</div>}
          </div>
        </div>
      ))}
    </>
  );
}

/* --- Services and tools -------------------------------------------------- */

function ServiceEditor({
  provider,
  services,
  onChange,
}: {
  provider: Provider;
  services: Services;
  onChange(next: Services): void;
}) {
  return (
    <div>
      {provider.services.map((info) => {
        const svc = services[info.key] ?? { enabled: false, tools: [] };
        const chosen = svc.tools ?? [];
        return (
          <div
            key={info.key}
            className={`svcblock ${svc.enabled ? "" : "svcblock--off"}`}
          >
            <div className="svcblock__head">
              <div className="svcblock__title">
                <span style={{ fontWeight: 500 }}>{info.label}</span>
                <span className="svcblock__desc">{info.description}</span>
              </div>
              <Switch
                on={svc.enabled}
                onChange={(on) =>
                  onChange({
                    ...services,
                    // Turning a service on grants its whole tool set; turning it
                    // off clears the list so nothing lingers server-side.
                    [info.key]: {
                      enabled: on,
                      tools: on ? info.tools.map((t) => t.name) : [],
                    },
                  })
                }
              />
            </div>
            {svc.enabled && (
              <div className="svcblock__tools">
                {info.tools.map((t) => (
                  <label className="svctool" key={t.name}>
                    <Checkbox
                      on={chosen.includes(t.name)}
                      onChange={(on) =>
                        onChange({
                          ...services,
                          [info.key]: {
                            enabled: true,
                            tools: on
                              ? [...chosen, t.name]
                              : chosen.filter((x) => x !== t.name),
                          },
                        })
                      }
                    />
                    <span className="svctool__body">
                      <span className="svctool__name">{t.name}</span>
                      <span className="svctool__desc">{t.description}</span>
                    </span>
                  </label>
                ))}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

/* --- Connected integration ----------------------------------------------- */

function IntegrationDetail({
  item,
  provider,
  onChanged,
  onDeleted,
}: {
  item: Integration;
  provider: Provider;
  onChanged(): void;
  onDeleted(): void;
}) {
  const [name, setName] = useState(item.name);
  const [enabled, setEnabled] = useState(item.enabled);
  const [services, setServices] = useState<Services>(item.services ?? {});
  const [modeValue, setModeValue] = useState(provider.modes[0].value);
  const [values, setValues] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [notice, setNotice] = useState<string>();
  const [confirmDelete, setConfirmDelete] = useState(false);

  const mode = provider.modes.find((m) => m.value === modeValue) ?? provider.modes[0];
  const needsCredentials = mode.fields.length > 0;

  const changed =
    name !== item.name ||
    enabled !== item.enabled ||
    JSON.stringify(services) !== JSON.stringify(item.services ?? {}) ||
    Object.values(values).some((v) => v.trim() !== "");

  const canSave =
    changed && name.trim() !== "" && (!needsCredentials || credentialsComplete(mode, values));

  async function save() {
    setBusy(true);
    setError(undefined);
    setNotice(undefined);
    try {
      await api.put<Integration>(`/integrations/${item.id}`, {
        name: name.trim(),
        type: item.type,
        enabled,
        credentials: buildCredentials(provider, mode, values),
        services,
      });
      setValues({});
      setNotice("Saved.");
      onChanged();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    setBusy(true);
    setError(undefined);
    try {
      await api.del(`/integrations/${item.id}`);
      onDeleted();
    } catch (err) {
      setError(describeError(err));
      setBusy(false);
    }
  }

  function revert() {
    setName(item.name);
    setEnabled(item.enabled);
    setServices(item.services ?? {});
    setValues({});
    setError(undefined);
    setNotice(undefined);
  }

  return (
    <>
      <div className="toolbar">
        <div className={`avatar avatar--${provider.tone}`} style={{ width: 22, height: 22 }}>
          <Icon name={provider.icon} size={13} />
        </div>
        <div className="toolbar__title">{item.name}</div>
        {item.authenticated ? (
          <span className="badge badge--green">Connected</span>
        ) : (
          <span className="badge badge--amber">Not connected</span>
        )}
        {!item.enabled && <span className="badge">Disabled</span>}
        <div className="spacer" />
        {confirmDelete ? (
          <span className="confirm">
            Remove {item.name}?
            <button className="btn btn--ghost" onClick={() => setConfirmDelete(false)}>
              Cancel
            </button>
            <button className="btn btn--danger" onClick={remove} disabled={busy}>
              Remove
            </button>
          </span>
        ) : (
          <button className="btn btn--danger" onClick={() => setConfirmDelete(true)}>
            <Icon name="trash" size={13} />
            Remove
          </button>
        )}
      </div>

      <div className="scroll" style={{ flex: 1, padding: "var(--sp-8)" }}>
        <div className="form">
          <AuthSection item={item} provider={provider} onChanged={onChanged} />

          {provider.supportsTriggers && (
            <>
              <div className="divider" />
              <TriggerRules integrationId={item.id} />
              <div className="divider" />
              <WebhookPanel integrationId={item.id} />
            </>
          )}

          <div className="divider" />

          <div className="formsec">
            <div className="formsec__title">Settings</div>
            <div className="formrow">
              <div className="formrow__label">Name</div>
              <div className="formrow__control">
                <label className="field">
                  <input
                    value={name}
                    onChange={(e) => setName(e.target.value)}
                    spellCheck={false}
                  />
                </label>
              </div>
            </div>
            <div className="formrow">
              <div className="formrow__label">Enabled</div>
              <div className="formrow__control">
                <div className="row" style={{ gap: "var(--sp-4)", alignItems: "center" }}>
                  <Switch on={enabled} onChange={setEnabled} />
                  <span style={{ fontSize: "var(--text-sm)", color: "var(--fg-secondary)" }}>
                    Expose this integration's tools to agents
                  </span>
                </div>
              </div>
            </div>
          </div>

          <div className="divider" />

          <div className="formsec">
            <div className="formsec__title">Services and tools</div>
            <ServiceEditor provider={provider} services={services} onChange={setServices} />
          </div>

          {needsCredentials && (
            <>
              <div className="divider" />
              <div className="formsec">
                <div className="formsec__title">Credentials</div>
                {/* The API replaces credentials wholesale on every update and
                    never sends the stored ones back, so a save that omitted
                    them would silently wipe the integration's secrets. */}
                <div className="msgline msgline--warn">
                  <span className="msgline__icon">
                    <Icon name="shield" size={13} />
                  </span>
                  <span>
                    Agento never returns stored secrets, and saving replaces them. Re-enter
                    the credentials below to save any change on this page — otherwise the
                    stored ones would be cleared.
                  </span>
                </div>
                {provider.modes.length > 1 && (
                  <div className="formrow">
                    <div className="formrow__label">Auth method</div>
                    <div className="formrow__control">
                      <Segmented
                        value={modeValue}
                        options={provider.modes.map((m) => ({
                          value: m.value,
                          label: m.label,
                        }))}
                        onChange={setModeValue}
                      />
                    </div>
                  </div>
                )}
                <CredentialFields mode={mode} values={values} onChange={setValues} />
              </div>
            </>
          )}

          {error && (
            <div className="msgline msgline--error">
              <span className="msgline__icon">
                <Icon name="alert" size={13} />
              </span>
              <span>{error}</span>
            </div>
          )}
          {notice && !changed && (
            <div className="msgline msgline--ok">
              <span className="msgline__icon">
                <Icon name="check" size={13} />
              </span>
              <span>{notice}</span>
            </div>
          )}

          {changed && (
            <div className="savebar">
              <span className="savebar__text">
                {canSave
                  ? "You have unsaved changes."
                  : needsCredentials
                    ? "Re-enter the credentials above to save."
                    : "A name is required."}
              </span>
              <button className="btn" onClick={revert} disabled={busy}>
                Revert
              </button>
              <button className="btn btn--primary" onClick={save} disabled={!canSave || busy}>
                {busy ? "Saving…" : "Save"}
              </button>
            </div>
          )}
        </div>
      </div>
    </>
  );
}

/* --- Auth ---------------------------------------------------------------- */

function AuthSection({
  item,
  provider,
  onChanged,
}: {
  item: Integration;
  provider: Provider;
  onChanged(): void;
}) {
  const isOAuth = provider.modes.some((m) => m.kind === "oauth");

  const [waiting, setWaiting] = useState(false);
  const [authUrl, setAuthUrl] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [notice, setNotice] = useState<string>();

  const onChangedRef = useRef(onChanged);
  onChangedRef.current = onChanged;

  // Poll for the redirect the provider makes back to the local callback.
  useEffect(() => {
    if (!waiting) return;
    const startedAt = Date.now();
    let cancelled = false;

    const id = setInterval(async () => {
      if (Date.now() - startedAt > 5 * 60_000) {
        if (!cancelled) {
          setWaiting(false);
          setError("Timed out waiting for authorisation.");
        }
        return;
      }
      try {
        const res = await api.get<{ authenticated: boolean }>(
          `/integrations/${item.id}/auth/status`
        );
        if (cancelled || !res.authenticated) return;
        setWaiting(false);
        setNotice("Authorised.");
        onChangedRef.current();
      } catch {
        /* transient — keep polling until the timeout */
      }
    }, 2000);

    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [waiting, item.id]);

  async function startOAuth() {
    setBusy(true);
    setError(undefined);
    setNotice(undefined);
    try {
      const res = await api.post<{ auth_url: string }>(
        `/integrations/${item.id}/auth/start`
      );
      setAuthUrl(res.auth_url);
      await openExternal(res.auth_url);
      setWaiting(true);
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  async function validate() {
    setBusy(true);
    setError(undefined);
    setNotice(undefined);
    try {
      const res = await api.post<{ valid: boolean; validated: boolean }>(
        `/integrations/${item.id}/auth/validate`
      );
      setNotice(
        res.validated
          ? "Credentials checked against the provider and accepted."
          : "Credentials stored. This provider has no live check, so they are not verified."
      );
      onChangedRef.current();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="formsec">
      <div className="formsec__title">Authorisation</div>
      <div className="formrow">
        <div className="formrow__label">Status</div>
        <div className="formrow__control">
          <div className="row" style={{ gap: "var(--sp-4)", alignItems: "center" }}>
            {item.authenticated ? (
              <span className="badge badge--green">Authenticated</span>
            ) : (
              <span className="badge badge--amber">Not authenticated</span>
            )}
            {isOAuth ? (
              <button className="btn" onClick={startOAuth} disabled={busy || waiting}>
                <Icon name="external" size={13} />
                {item.authenticated ? "Re-authorise" : "Authorise"}
              </button>
            ) : (
              <button className="btn" onClick={validate} disabled={busy}>
                <Icon name="check" size={13} />
                {busy ? "Checking…" : "Validate credentials"}
              </button>
            )}
          </div>

          {isOAuth && (
            <div className="formrow__help">
              Authorisation opens in your normal browser, not inside this window.
            </div>
          )}
          {!isOAuth && (
            <div className="formrow__help">
              Checks the credentials already saved on the server, not any unsaved edits
              below.
            </div>
          )}

          {waiting && (
            <div className="msgline">
              <span className="msgline__icon">
                <Icon name="clock" size={13} />
              </span>
              <span style={{ flex: 1 }}>
                Waiting for you to finish in the browser…
                {authUrl && (
                  <>
                    {" "}
                    <button
                      className="btn btn--ghost"
                      style={{ height: 18, padding: "0 var(--sp-3)" }}
                      onClick={() => authUrl && openExternal(authUrl)}
                    >
                      Reopen link
                    </button>
                  </>
                )}
              </span>
              <button className="btn" onClick={() => setWaiting(false)}>
                Cancel
              </button>
            </div>
          )}
          {error && <div className="msgline msgline--error">{error}</div>}
          {notice && <div className="msgline msgline--ok">{notice}</div>}
        </div>
      </div>
    </div>
  );
}

/* --- Telegram trigger rules ---------------------------------------------- */

interface RuleDraft {
  id?: string;
  name: string;
  agent_slug: string;
  enabled: boolean;
  filter_prefix: string;
  filter_keywords: string;
  filter_chat_ids: string;
}

const BLANK_RULE: RuleDraft = {
  name: "",
  agent_slug: "",
  enabled: true,
  filter_prefix: "",
  filter_keywords: "",
  filter_chat_ids: "",
};

function splitList(v: string): string[] | null {
  const parts = v
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  return parts.length > 0 ? parts : null;
}

function TriggerRules({ integrationId }: { integrationId: string }) {
  const rules = useResource(
    (signal) =>
      api.get<TriggerRule[] | null>(`/integrations/${integrationId}/triggers`, signal),
    [integrationId]
  );
  const agents = useResource(
    (signal) => api.get<Agent[] | null>("/agents", signal),
    []
  );

  const [draft, setDraft] = useState<RuleDraft>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [confirmDelete, setConfirmDelete] = useState<string>();

  const list = rules.data ?? [];
  const agentList = agents.data ?? [];

  async function run(action: () => Promise<unknown>) {
    setBusy(true);
    setError(undefined);
    try {
      await action();
      rules.reload();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  function save(d: RuleDraft) {
    const body = {
      name: d.name.trim(),
      agent_slug: d.agent_slug.trim(),
      enabled: d.enabled,
      filter_prefix: d.filter_prefix.trim(),
      filter_keywords: splitList(d.filter_keywords),
      filter_chat_ids: splitList(d.filter_chat_ids),
    };
    return run(async () => {
      if (d.id) {
        await api.put(`/integrations/${integrationId}/triggers/${d.id}`, body);
      } else {
        await api.post(`/integrations/${integrationId}/triggers`, body);
      }
      setDraft(undefined);
    });
  }

  return (
    <div className="formsec">
      <div className="formsec__title">Trigger rules</div>
      <div className="formrow__help">
        An inbound message matching a rule starts the chosen agent. Rules are evaluated in
        order and a message only fires the first that matches.
      </div>

      <div className="grouplist">
        {list.length === 0 && !draft && (
          <div className="rulerow">
            <span style={{ color: "var(--fg-tertiary)", fontSize: "var(--text-sm)" }}>
              {rules.loading ? "Loading…" : "No rules — inbound messages are ignored."}
            </span>
          </div>
        )}

        {list.map((r) =>
          draft?.id === r.id ? (
            <RuleForm
              key={r.id}
              draft={draft}
              agents={agentList}
              busy={busy}
              onChange={setDraft}
              onCancel={() => setDraft(undefined)}
              onSave={() => save(draft)}
            />
          ) : (
            <div className="rulerow" key={r.id}>
              <div className="rulerow__body">
                <span style={{ fontWeight: 500 }}>{r.name || "Untitled rule"}</span>
                <span className="rulerow__meta">
                  <span>→ {r.agent_slug || "no agent"}</span>
                  {r.filter_prefix && <span>prefix “{r.filter_prefix}”</span>}
                  {r.filter_keywords && r.filter_keywords.length > 0 && (
                    <span>keywords: {r.filter_keywords.join(", ")}</span>
                  )}
                  {r.filter_chat_ids && r.filter_chat_ids.length > 0 && (
                    <span>{r.filter_chat_ids.length} chat filter(s)</span>
                  )}
                </span>
              </div>
              {confirmDelete === r.id ? (
                <span className="confirm">
                  Delete rule?
                  <button className="btn btn--ghost" onClick={() => setConfirmDelete(undefined)}>
                    Cancel
                  </button>
                  <button
                    className="btn btn--danger"
                    disabled={busy}
                    onClick={() =>
                      run(async () => {
                        await api.del(`/integrations/${integrationId}/triggers/${r.id}`);
                        setConfirmDelete(undefined);
                      })
                    }
                  >
                    Delete
                  </button>
                </span>
              ) : (
                <>
                  <Switch
                    on={r.enabled}
                    onChange={(on) =>
                      run(() =>
                        api.put(`/integrations/${integrationId}/triggers/${r.id}`, {
                          name: r.name,
                          agent_slug: r.agent_slug,
                          enabled: on,
                          filter_prefix: r.filter_prefix,
                          filter_keywords: r.filter_keywords,
                          filter_chat_ids: r.filter_chat_ids,
                        })
                      )
                    }
                  />
                  <button
                    className="iconbtn"
                    title="Edit"
                    onClick={() =>
                      setDraft({
                        id: r.id,
                        name: r.name,
                        agent_slug: r.agent_slug,
                        enabled: r.enabled,
                        filter_prefix: r.filter_prefix,
                        filter_keywords: (r.filter_keywords ?? []).join(", "),
                        filter_chat_ids: (r.filter_chat_ids ?? []).join(", "),
                      })
                    }
                  >
                    <Icon name="edit" size={13} />
                  </button>
                  <button
                    className="iconbtn"
                    title="Delete"
                    onClick={() => setConfirmDelete(r.id)}
                  >
                    <Icon name="trash" size={13} />
                  </button>
                </>
              )}
            </div>
          )
        )}

        {draft && !draft.id && (
          <RuleForm
            draft={draft}
            agents={agentList}
            busy={busy}
            onChange={setDraft}
            onCancel={() => setDraft(undefined)}
            onSave={() => save(draft)}
          />
        )}
      </div>

      {!draft && (
        <button
          className="btn"
          style={{ alignSelf: "flex-start" }}
          onClick={() => setDraft({ ...BLANK_RULE })}
        >
          <Icon name="plus" size={13} />
          New rule
        </button>
      )}

      {error && <div className="msgline msgline--error">{error}</div>}
    </div>
  );
}

function RuleForm({
  draft,
  agents,
  busy,
  onChange,
  onCancel,
  onSave,
}: {
  draft: RuleDraft;
  agents: Agent[];
  busy: boolean;
  onChange(d: RuleDraft): void;
  onCancel(): void;
  onSave(): void;
}) {
  const ready = draft.name.trim() !== "" && draft.agent_slug.trim() !== "";

  return (
    <div className="rulerow" style={{ flexDirection: "column", alignItems: "stretch", gap: "var(--sp-4)" }}>
      <div className="row" style={{ gap: "var(--sp-3)" }}>
        <label className="field field--sm" style={{ flex: 1 }}>
          <input
            autoFocus
            value={draft.name}
            onChange={(e) => onChange({ ...draft, name: e.target.value })}
            placeholder="Rule name"
            spellCheck={false}
          />
        </label>
        {agents.length > 0 ? (
          <Dropdown
            small
            value={draft.agent_slug}
            onChange={(v) => onChange({ ...draft, agent_slug: v })}
            placeholder="Choose an agent…"
            options={[
              { value: "", label: "Choose an agent…" },
              ...agents.map((a) => ({ value: a.slug, label: a.name })),
            ]}
          />
        ) : (
          <label className="field field--sm" style={{ flex: 1 }}>
            <input
              value={draft.agent_slug}
              onChange={(e) => onChange({ ...draft, agent_slug: e.target.value })}
              placeholder="agent-slug"
              className="mono"
              spellCheck={false}
            />
          </label>
        )}
      </div>

      <div className="row" style={{ gap: "var(--sp-3)" }}>
        <label className="field field--sm" style={{ flex: 1 }}>
          <input
            value={draft.filter_prefix}
            onChange={(e) => onChange({ ...draft, filter_prefix: e.target.value })}
            placeholder="Prefix, e.g. /ask"
            spellCheck={false}
          />
        </label>
        <label className="field field--sm" style={{ flex: 1 }}>
          <input
            value={draft.filter_keywords}
            onChange={(e) => onChange({ ...draft, filter_keywords: e.target.value })}
            placeholder="Keywords, comma separated"
            spellCheck={false}
          />
        </label>
      </div>

      <div className="row" style={{ gap: "var(--sp-3)" }}>
        <label className="field field--sm" style={{ flex: 1 }}>
          <input
            value={draft.filter_chat_ids}
            onChange={(e) => onChange({ ...draft, filter_chat_ids: e.target.value })}
            placeholder="Chat IDs, comma separated (blank = any chat)"
            className="mono"
            spellCheck={false}
          />
        </label>
      </div>

      <div className="row" style={{ gap: "var(--sp-4)", alignItems: "center" }}>
        <Switch on={draft.enabled} onChange={(v) => onChange({ ...draft, enabled: v })} />
        <span style={{ fontSize: "var(--text-sm)", color: "var(--fg-secondary)", flex: 1 }}>
          Enabled
        </span>
        <button className="btn btn--ghost" onClick={onCancel}>
          Cancel
        </button>
        <button className="btn btn--primary" onClick={onSave} disabled={!ready || busy}>
          {draft.id ? "Save rule" : "Add rule"}
        </button>
      </div>
    </div>
  );
}

/* --- Telegram webhook ---------------------------------------------------- */

function WebhookPanel({ integrationId }: { integrationId: string }) {
  const status = useResource(
    (signal) =>
      api.get<WebhookStatus>(`/integrations/${integrationId}/webhook/status`, signal),
    [integrationId]
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [confirmRemove, setConfirmRemove] = useState(false);

  async function run(action: () => Promise<unknown>) {
    setBusy(true);
    setError(undefined);
    try {
      await action();
      status.reload();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  const s = status.data;
  const active = s?.status === "active";

  return (
    <div className="formsec">
      <div className="formsec__title">Webhook</div>
      <div className="formrow">
        <div className="formrow__label">Delivery</div>
        <div className="formrow__control">
          <div className="row" style={{ gap: "var(--sp-4)", alignItems: "center" }}>
            {active ? (
              <span className="badge badge--green">Active</span>
            ) : s?.status === "error" ? (
              <span className="badge badge--red">Error</span>
            ) : (
              <span className="badge">Inactive</span>
            )}
            {s?.has_secret && <span className="badge">Secret set</span>}
            {!active && (
              <button
                className="btn"
                disabled={busy}
                onClick={() =>
                  run(() => api.post(`/integrations/${integrationId}/webhook/register`))
                }
              >
                Register
              </button>
            )}
            {active && (
              <>
                <button
                  className="btn"
                  disabled={busy}
                  onClick={() =>
                    run(() =>
                      api.post(`/integrations/${integrationId}/webhook/regenerate-secret`)
                    )
                  }
                >
                  <Icon name="refresh" size={13} />
                  Regenerate secret
                </button>
                {confirmRemove ? (
                  <span className="confirm">
                    Remove webhook?
                    <button className="btn btn--ghost" onClick={() => setConfirmRemove(false)}>
                      Cancel
                    </button>
                    <button
                      className="btn btn--danger"
                      disabled={busy}
                      onClick={() =>
                        run(async () => {
                          await api.del(
                            `/integrations/${integrationId}/webhook/register`
                          );
                          setConfirmRemove(false);
                        })
                      }
                    >
                      Remove
                    </button>
                  </span>
                ) : (
                  <button className="btn btn--danger" onClick={() => setConfirmRemove(true)}>
                    Remove
                  </button>
                )}
              </>
            )}
          </div>

          <div className="formrow__help">
            Telegram pushes updates to this instance. Registering needs a reachable public
            URL, set under Settings → General.
          </div>

          {s?.url && <div className="codebox">{s.url}</div>}
          {s?.error && <div className="msgline msgline--error">{s.error}</div>}
          {error && <div className="msgline msgline--error">{error}</div>}
        </div>
      </div>
    </div>
  );
}

/* --- Inspector ----------------------------------------------------------- */

function ConnectedInspector({
  item,
  provider,
  tools,
}: {
  item: Integration;
  provider: Provider | undefined;
  tools: AvailableTool[];
}) {
  const mine = tools.filter((t) => t.integration_id === item.id);
  const services = Object.entries(item.services ?? {});
  const enabledServices = services.filter(([, s]) => s.enabled);

  return (
    <>
      <InspGroup title="Status">
        <InspRow label="State">
          {item.authenticated ? (
            <span className="badge badge--green">Connected</span>
          ) : (
            <span className="badge badge--amber">Not authenticated</span>
          )}
        </InspRow>
        <InspRow label="Enabled">{item.enabled ? "Yes" : "No"}</InspRow>
        <InspRow label="Provider">{provider?.label ?? item.type}</InspRow>
        <InspRow label="Auth">{provider?.modes[0].label ?? "—"}</InspRow>
      </InspGroup>

      <InspGroup title="Access">
        <InspRow label="Services">
          <span className="tnum">
            {enabledServices.length} / {services.length || provider?.services.length || 0}
          </span>
        </InspRow>
        <InspRow label="Tools granted">
          <span className="tnum">{countTools(item.services)}</span>
        </InspRow>
        <InspRow label="Live tools">
          <span className="tnum">{mine.length}</span>
        </InspRow>
      </InspGroup>

      {enabledServices.length > 0 && (
        <InspGroup title="Enabled services">
          <div style={{ display: "flex", flexWrap: "wrap", gap: 4 }}>
            {enabledServices.map(([key]) => (
              <span key={key} className="badge">
                {provider?.services.find((s) => s.key === key)?.label ?? key}
              </span>
            ))}
          </div>
        </InspGroup>
      )}

      <InspGroup title="History">
        <InspRow label="Added">
          <span title={dateTime(item.created_at)}>{relativeTime(item.created_at)}</span>
        </InspRow>
        <InspRow label="Updated">
          <span title={dateTime(item.updated_at)}>{relativeTime(item.updated_at)}</span>
        </InspRow>
        <InspRow label="ID">
          <span className="mono" style={{ fontSize: "var(--text-xs)" }}>
            {item.id}
          </span>
        </InspRow>
      </InspGroup>
    </>
  );
}

function ProviderInspector({ provider }: { provider: Provider }) {
  const toolCount = provider.services.reduce((n, s) => n + s.tools.length, 0);
  return (
    <>
      <InspGroup title="Status">
        <InspRow label="State">
          <span className="badge">Not set up</span>
        </InspRow>
        <InspRow label="Provider">{provider.label}</InspRow>
        <InspRow label="Auth">
          {provider.modes.map((m) => m.label).join(" or ")}
        </InspRow>
      </InspGroup>
      <InspGroup title="Offers">
        <InspRow label="Services">
          <span className="tnum">{provider.services.length}</span>
        </InspRow>
        <InspRow label="Tools">
          <span className="tnum">{toolCount}</span>
        </InspRow>
        <InspRow label="Triggers">{provider.supportsTriggers ? "Yes" : "No"}</InspRow>
      </InspGroup>
      <InspGroup title="Services">
        <div style={{ display: "flex", flexWrap: "wrap", gap: 4 }}>
          {provider.services.map((s) => (
            <span key={s.key} className="badge">
              {s.label}
            </span>
          ))}
        </div>
      </InspGroup>
    </>
  );
}
