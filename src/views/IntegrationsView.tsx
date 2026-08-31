import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { api } from "../lib/api";
import { describeError, useResource } from "../lib/hooks";
import { dateTime, relativeTime } from "../lib/format";
import { partnerLabel, submitLabel, SUBMIT_CREATE } from "../lib/formVerbs";
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
  modeFor,
  PROVIDERS,
  providerFor,
  unavailableCopy,
  type AuthMode,
  type Provider,
} from "./integrations/catalog";
import "../styles/integrations.css";

/* ============================================================================
   Integrations — connect a provider, then choose what its agents may call.

   **The connect screen and the edit screen render the same form** (#517).
   `ConnectForm` and `IntegrationDetail` were written independently and every
   wrapper around the two shared pieces (`CredentialFields`, `ServiceEditor`)
   drifted: the same Name field sat under two different headings, in two
   different positions, and the one sentence explaining what the checkbox grid
   means was shown only to people who had never used the app before.

   The rule that replaced it, and the three things that keep it true:

   * **`ConnectionFields` and `ServicesSection` are the shared body**, composed
     by both screens. A field cannot appear on one and not the other, and it
     cannot move on one screen alone.
   * **Every heading and help string below is a module constant.** Two screens
     spelling one heading twice is exactly how the drift started, so a rename
     here changes both.
   * **Edit-only sections render *after* the shared body** — Authorisation, the
     trigger rules, the webhook and the *Enabled* switch. Both screens therefore
     open on the same thing, and the layout a user learns on the way in is the
     one they meet on the way back.
   ========================================================================== */

/** The heading over Name, the auth method and the credential fields. */
const CONNECTION_TITLE = "Connection";
/** The heading over the service/tool grid. */
const SERVICES_TITLE = "Services and tools";
/** The heading over the edit-only *Enabled* switch. */
const AVAILABILITY_TITLE = "Availability";

const NAME_HELP =
  "How this connection is labelled in Agento. Useful when you connect the same provider twice.";
const SERVICES_HELP =
  "Only the tools you leave on are exposed to agents. You can change this later.";
const STORED_SECRET_HELP =
  "Agento cannot show a stored secret back to you, and does not need to — leave this alone and saving keeps it.";

type Selection =
  | {
      kind: "integration";
      id: string;
      /**
       * A credential rejection the *create* form could not report, because it
       * unmounts as this selection is made. One-shot: the detail pane seeds its
       * own error from it and never reads it again, which is why it lives on
       * the selection rather than beside the row.
       */
      authError?: string;
    }
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

/**
 * The credentials blob for a *save*, or `undefined` when the user typed
 * nothing (#515).
 *
 * `PUT /api/integrations/{id}` is three-valued: an absent `credentials` key
 * leaves the stored blob alone, and any present value — `{}` included —
 * replaces it. So "the user did not touch the credential fields" has to be
 * spelled as *sending no key*, which is what `undefined` becomes when the
 * request object is spread. Building the blob unconditionally is what used to
 * make every save a wipe, and it is why the field had its own quarantined
 * section behind a warning.
 *
 * This is `ProvidersView`'s `...(hasKey ? { api_key } : {})`, one level up:
 * there the credential is one string, here it is a map, so "typed nothing" is
 * "no field of the selected mode has a value".
 */
function credentialsToSave(
  provider: Provider,
  mode: AuthMode,
  values: Record<string, string>
): Record<string, string> | undefined {
  return hasTypedCredentials(mode, values)
    ? buildCredentials(provider, mode, values)
    : undefined;
}

function credentialsComplete(mode: AuthMode, values: Record<string, string>): boolean {
  return mode.fields.every((f) => (values[f.key] ?? "").trim() !== "");
}

/**
 * Whether the user has supplied a credential at all — the predicate that
 * decides both whether a save *sends* a `credentials` key and whether the form
 * insists the blob be complete before it will.
 *
 * One function rather than the same `.some()` written at both sites: the two
 * are only correct while they agree, and a gate that disagrees with what is
 * actually sent is either a half-filled blob saved as a credential or a save
 * button that never enables. This is the frontend half of the rule
 * `the_two_has_credentials_rules_agree` pins on the backend.
 */
function hasTypedCredentials(mode: AuthMode, values: Record<string, string>): boolean {
  return mode.fields.some((f) => (values[f.key] ?? "").trim() !== "");
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
            onCreated={(id, authError) => {
              reloadAll();
              setSelection({ kind: "integration", id, authError });
            }}
          />
        ) : selected && selectedProvider ? (
          <IntegrationDetail
            key={selected.id}
            item={selected}
            provider={selectedProvider}
            initialError={selection?.kind === "integration" ? selection.authError : undefined}
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

/* --- The action strip ---------------------------------------------------- */

/**
 * The one place either half of this view submits from — and the reason it is a
 * component rather than two copies of the same JSX (#516).
 *
 * Integrations used to be the only view whose primary action *moved*: a
 * `+ Create` in the top toolbar while connecting, a `Save` in the bottom
 * savebar while editing. One strip rendered from both components is what makes
 * a future divergence a deliberate edit rather than an oversight.
 *
 * **The grammar it encodes is repo-wide, not this view's** — see *Conventions*
 * in `CLAUDE.md`, which is where it is written down for the views that do not
 * import this file:
 *
 * - **Primary verb follows existence**: `Create` while the record does not
 *   exist yet, `Save` once it does. The in-flight label follows it.
 * - **Partner follows the same split**: `Discard` throws away a record that
 *   was never stored, `Revert` restores one that was. Two words because they
 *   undo two different things; a single "Cancel" would claim to undo a save.
 * - **No `+` icon.** The `+` marks a list-level *New X* that opens a blank
 *   thing; on a submit it reads as "add another one", which is the confusion
 *   this issue was reported for.
 *
 * `message` is the savebar's own explanation of why the primary is disabled,
 * so a form never leaves the user guessing at a greyed-out button.
 */
function SaveBar({
  creating,
  busy,
  canSubmit,
  message,
  onDiscard,
  onSubmit,
}: {
  creating: boolean;
  busy: boolean;
  canSubmit: boolean;
  message: string;
  onDiscard(): void;
  onSubmit(): void;
}) {
  return (
    <div className="savebar">
      <span className="savebar__text">{message}</span>
      <button className="btn" onClick={onDiscard} disabled={busy}>
        {partnerLabel(creating)}
      </button>
      <button
        className="btn btn--primary"
        onClick={onSubmit}
        disabled={!canSubmit || busy}
      >
        {submitLabel(creating, busy)}
      </button>
    </div>
  );
}

/* --- Connect a new integration ------------------------------------------- */

function ConnectForm({
  provider,
  onCreated,
}: {
  provider: Provider;
  /**
   * `authError` is the credential check's refusal, when there was one. The row
   * exists either way, so it travels to the detail pane rather than keeping
   * this form mounted — see [`create`].
   */
  onCreated(id: string, authError?: string): void;
}) {
  const [name, setName] = useState(provider.label);
  const [modeValue, setModeValue] = useState(provider.modes[0].value);
  const [values, setValues] = useState<Record<string, string>>({});
  const [services, setServices] = useState<Services>(() => emptyServices(provider, true));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  const mode = modeFor(provider, modeValue);
  const ready = name.trim() !== "" && credentialsComplete(mode, values);

  async function create() {
    setBusy(true);
    setError(undefined);
    try {
      const created = await api.post<Integration>("/integrations", {
        name: name.trim(),
        type: provider.type,
        // **Deliberately not offered on this screen** (#517), which is why the
        // *Availability* switch is the one control the edit screen has and this
        // one does not. Connecting a provider is an act of turning it on;
        // a switch here would only ever be used to create something inert, and
        // the edit screen is one click away for the day that changes.
        enabled: true,
        credentials: buildCredentials(provider, mode, values),
        services,
      });
      // Same rule as `IntegrationDetail.save()`: a token credential is only
      // usable once the check has written the `auth` column, so connecting is
      // one action.
      //
      // A refusal is **carried, not swallowed** — the provider's own sentence
      // (`slack API error: invalid_auth`) is the only thing that tells the user
      // what went wrong, and it must not die with this form. The row was
      // created and must not be orphaned, so the error travels to the detail
      // pane instead of keeping the user here.
      if (mode.kind === "token") {
        try {
          await api.post(`/integrations/${created.id}/auth/validate`);
        } catch (err) {
          // The same honesty the save path owes: the user is moved out of this
          // form into a pane for a row they may not realise now exists, so the
          // message has to say that the integration *was* created and is simply
          // not connected.
          onCreated(
            created.id,
            `${describeError(err)} — the integration was created, but the credential was not accepted, so it is not connected.`
          );
          return;
        }
      }
      onCreated(created.id);
    } catch (err) {
      setError(describeError(err));
      setBusy(false);
    }
  }

  /**
   * Throw away everything typed so far without leaving the screen — the
   * connect form's half of the `Discard`/`Revert` pair. There is no stored
   * record to restore, so "back" means the state this form opened in, and
   * `values` is emptied outright because it is the one field that can hold a
   * secret the user pasted by mistake.
   */
  function discard() {
    setName(provider.label);
    setModeValue(provider.modes[0].value);
    setValues({});
    setServices(emptyServices(provider, true));
    setError(undefined);
  }

  return (
    <>
      <div className="toolbar">
        <div className={`avatar avatar--${provider.tone}`} style={{ width: 22, height: 22 }}>
          <Icon name={provider.icon} size={13} />
        </div>
        <div className="toolbar__title">Connect {provider.label}</div>
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

          <ConnectionFields
            provider={provider}
            name={name}
            onName={setName}
            mode={mode}
            modeValue={modeValue}
            onMode={setModeValue}
            values={values}
            onValues={setValues}
            note={
              mode.kind === "oauth" && (
                <div className="msgline">
                  <span className="msgline__icon">
                    <Icon name="info" size={13} />
                  </span>
                  <span>
                    After the integration is created you will be sent to {provider.label} in
                    your browser to authorise it.
                  </span>
                </div>
              )
            }
          />

          <div className="divider" />

          <ServicesSection provider={provider} services={services} onChange={setServices} />

          {error && (
            <div className="msgline msgline--error">
              <span className="msgline__icon">
                <Icon name="alert" size={13} />
              </span>
              <span>{error}</span>
            </div>
          )}

          {/* Always rendered, where the edit screen shows its strip only once
              something changed: nothing here has been stored, so there is no
              state in which the primary action should be absent. */}
          <SaveBar
            creating
            busy={busy}
            canSubmit={ready}
            message={
              ready
                ? // Interpolated rather than spelled out, so the sentence
                  // cannot drift away from the button it names.
                  `Not connected yet — ${provider.label} is added when you press ${SUBMIT_CREATE}.`
                : name.trim() === ""
                  ? "A name is required."
                  : "Fill in every credential field."
            }
            onDiscard={discard}
            onSubmit={create}
          />
        </div>
      </div>
    </>
  );
}

/* --- The shared form body ------------------------------------------------ */

/**
 * Name, the auth method and the credential fields — the block both screens
 * open on (#517).
 *
 * `stored` is the one difference between them, and it is a difference in what
 * exists rather than in layout: only a saved integration can have a secret to
 * keep, so only the edit screen passes it. While it is present the masked line
 * stands in for the auth method and the fields, because the method rides
 * *inside* the credential blob and offering it beside a secret nobody is
 * replacing would let the user change something that sends nothing — see
 * `IntegrationDetail`'s note on `canSave`.
 */
function ConnectionFields({
  provider,
  name,
  onName,
  mode,
  modeValue,
  onMode,
  values,
  onValues,
  stored,
  note,
}: {
  provider: Provider;
  name: string;
  onName(next: string): void;
  mode: AuthMode;
  modeValue: string;
  onMode(next: string): void;
  values: Record<string, string>;
  onValues(next: Record<string, string>): void;
  /**
   * Edit-only, and present only while a stored secret is being kept: renders
   * `••• stored` behind an explicit *Replace* in place of the auth method and
   * the fields (#515).
   */
  stored?: { onReplace(): void };
  /** Anything the screen wants at the foot of the section. */
  note?: ReactNode;
}) {
  /**
   * An OAuth provider that carries no credential fields at all has nothing to
   * mask, so the stored-secret line is gated on this — the block is then Name
   * alone rather than an empty headed section.
   *
   * **The auth-method control is deliberately *not* gated on it.** It is the
   * only way to reach a method that does have fields, so hiding it for a
   * fieldless mode would strand the form on that mode (`modeValue` seeds to
   * `provider.modes[0]`, and nothing else changes it). `CredentialFields`
   * needs no gate at all: it maps over `mode.fields` and renders nothing for
   * an empty one. Unreachable with today's catalogue — every mode of all six
   * providers carries at least one field — and cheap to keep right.
   */
  const needsCredentials = mode.fields.length > 0;

  return (
    <div className="formsec">
      <div className="formsec__title">{CONNECTION_TITLE}</div>
      <div className="formrow">
        <div className="formrow__label">Name</div>
        <div className="formrow__control">
          <label className="field">
            <input
              value={name}
              onChange={(e) => onName(e.target.value)}
              placeholder={provider.label}
              spellCheck={false}
            />
          </label>
          <div className="formrow__help">{NAME_HELP}</div>
        </div>
      </div>

      {stored ? (
        needsCredentials && (
          /* The label stays generic rather than becoming `mode.label`. The
             stored mode *is* reportable since #513, and the inspector reports
             it — but only when the row records one. A multi-mode row saved
             before that field existed records nothing, and `modeValue` falls
             back to `provider.modes[0]` for it, so a Slack row connected by
             OAuth would still be captioned "Bot token" here. As a caption on a
             stored secret that is a claim the app cannot make for every row,
             and this one caption serves all of them. */
          <div className="formrow">
            <div className="formrow__label">Credentials</div>
            <div className="formrow__control">
              <div className="int-storedsecret">
                <span className="mono">••••••••••• stored</span>
                <button className="btn btn--ghost" onClick={stored.onReplace}>
                  Replace
                </button>
              </div>
              <div className="formrow__help">{STORED_SECRET_HELP}</div>
            </div>
          </div>
        )
      ) : (
        <>
          {provider.modes.length > 1 && (
            <div className="formrow">
              <div className="formrow__label">Auth method</div>
              <div className="formrow__control">
                <Segmented
                  value={modeValue}
                  options={provider.modes.map((m) => ({ value: m.value, label: m.label }))}
                  onChange={onMode}
                />
              </div>
            </div>
          )}

          <CredentialFields mode={mode} values={values} onChange={onValues} />
        </>
      )}

      {note}
    </div>
  );
}

/**
 * The service/tool grid under its heading, with the one sentence that explains
 * what the grid means (#517) — which used to render on the connect screen only,
 * so the people who had to live with the choice were the ones never told what
 * it did.
 */
function ServicesSection({
  provider,
  services,
  onChange,
}: {
  provider: Provider;
  services: Services;
  onChange(next: Services): void;
}) {
  return (
    <div className="formsec">
      <div className="formsec__title">{SERVICES_TITLE}</div>
      <div className="formrow__help">{SERVICES_HELP}</div>
      <ServiceEditor provider={provider} services={services} onChange={onChange} />
    </div>
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
        const stored = svc.tools ?? [];
        // What this service actually exposes today, which is not the same as
        // what it stores (#501). Three shapes have to be translated:
        //
        // - **disabled** exposes nothing whatever its list says, so the boxes
        //   read empty — otherwise a row saved as
        //   `{enabled: false, tools: [...]}` shows ticks for tools no agent can
        //   reach, and checking one more silently restores the rest.
        // - **enabled with no list, and no sibling has one either** exposes
        //   *every* tool of the group. Rendering that as nothing ticked is the
        //   exact inverse of the truth, and it is the shape
        //   `POST /api/integrations` and every pre-list row carry — so it is
        //   what a user is most likely to be looking at.
        // - **enabled with no list while a sibling has one** exposes
        //   **nothing**, and this is the case that reads backwards. "Host
        //   everything" is a property of `build_allowed_set`, which unions
        //   *every* enabled service across the whole integration — not of one
        //   group being empty. So a listless `gmail` beside a
        //   `drive: ["list_files"]` matches no name in that union and hosts
        //   zero tools. Ticking it here would not only misreport it: the union
        //   is what makes unchecking one box *grant* the other two.
        // Over the **stored** map, not `provider.services`: `build_allowed_set`
        // unions `services.values()`, so a key outside this app's catalog — one
        // `POST /api/integrations` accepted, since it validates no service
        // names — contributes to the real union while a catalog walk cannot see
        // it. Same answer for every catalog key, since an absent service
        // contributes nothing either way.
        const unionEmpty = Object.values(services).every(
          (other) => !other?.enabled || !other.tools?.length,
        );
        const chosen = !svc.enabled
          ? []
          : stored.length
            ? stored
            : unionEmpty
              ? info.tools.map((t) => t.name)
              : [];
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
            {/* The checkboxes stay mounted when the service is off, and that is
                not cosmetic (#501): unchecking the last tool now turns the
                service off, so hiding them would make the state it lands in a
                dead end — going from "only get_repo" to "only list_repos" would
                mean re-enabling the service, which grants *every* tool. The
                block is already greyed by `svcblock--off`. */}
            <div className="svcblock__tools">
              {info.tools.map((t) => (
                <label className="svctool" key={t.name}>
                  <Checkbox
                    on={chosen.includes(t.name)}
                    onChange={(on) => {
                      const next = on
                        ? [...chosen, t.name]
                        : chosen.filter((x) => x !== t.name);
                      onChange({
                        ...services,
                        // Unchecking the **last** tool turns the service off
                        // rather than storing `{enabled: true, tools: []}`
                        // (#501). The backend reads an empty tool list as
                        // "host everything" — the semantics are ported and
                        // pinned in all six integrations — so that shape means
                        // the exact opposite of what the user just asked for,
                        // and of what the copy above this editor promises.
                        // Checking one is the way back on.
                        [info.key]: {
                          enabled: next.length > 0,
                          tools: next,
                        },
                      });
                    }}
                  />
                  <span className="svctool__body">
                    <span className="svctool__name">{t.name}</span>
                    <span className="svctool__desc">{t.description}</span>
                  </span>
                </label>
              ))}
            </div>
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
  initialError,
  onChanged,
  onDeleted,
}: {
  item: Integration;
  provider: Provider;
  /** The create form's credential rejection, when this pane opened onto one. */
  initialError?: string;
  onChanged(): void;
  onDeleted(): void;
}) {
  const [name, setName] = useState(item.name);
  const [enabled, setEnabled] = useState(item.enabled);
  const [services, setServices] = useState<Services>(item.services ?? {});
  // Seeded from what the row actually records, so reopening a saved bot-token
  // Slack integration lands on the tab the user picked rather than on the
  // provider's first mode by coincidence.
  const [modeValue, setModeValue] = useState(() => modeFor(provider, item.auth_mode).value);
  const [values, setValues] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(initialError);
  const [notice, setNotice] = useState<string>();
  const [confirmDelete, setConfirmDelete] = useState(false);

  const mode = modeFor(provider, modeValue);
  /**
   * `find`, deliberately, where everything else here uses `modeFor`.
   *
   * The case it protects is the **multi-mode row that records no mode** — a
   * Slack integration saved before `auth_mode` reached the wire. `modeFor` would
   * resolve that to `bot_token`, and the gate below would then read the OAuth
   * tab as an unsaved switch and disable *Authorise* for a row whose stored mode
   * is genuinely unknown. `undefined` is the honest answer, and it turns the
   * gate off in both tabs, which is the behaviour such a row had before the gate
   * existed. (Single-mode providers cannot reach the gate at all: the Auth
   * method control only renders at `modes.length > 1`, so `mode` never leaves
   * `modes[0]`.)
   */
  const storedMode = provider.modes.find((m) => m.value === item.auth_mode);
  const needsCredentials = mode.fields.length > 0;
  /**
   * Whether the credential inputs are shown at all (#515). A stored secret
   * renders as `••• stored` behind an explicit *Replace*, because Agento
   * cannot show it back and no longer needs to: an untouched field sends no
   * `credentials` key and the stored blob survives.
   *
   * It starts open when there is nothing stored, which is the one case where
   * leaving the field alone keeps the integration unusable.
   */
  const [replacing, setReplacing] = useState(!item.has_credentials);
  const typedCredentials = hasTypedCredentials(mode, values);
  /** The Auth method control is on a method the row does not record. */
  const modeChanged = modeValue !== modeFor(provider, item.auth_mode).value;

  const changed =
    name !== item.name ||
    enabled !== item.enabled ||
    // The Auth method control is an edit like any other. Leaving it out left the
    // savebar unrendered while `AuthSection`'s gate told the user to save —
    // greyed actions, and no Save button anywhere to press.
    modeChanged ||
    JSON.stringify(services) !== JSON.stringify(item.services ?? {}) ||
    typedCredentials;

  // A credential is required only when the user is actually supplying one:
  // a half-filled mode would be saved as a blob with empty values, which is a
  // broken credential rather than a preserved one. Not typing anything is now
  // a complete, valid save — that is the whole point of #515.
  //
  // **A mode switch is the exception, and it is #515 meeting #513.** `auth_mode`
  // rides *inside* `credentials`, and #515 only sends that key when something
  // was typed — so a switch on its own would send nothing, change nothing, and
  // report "Saved.", leaving the control on a method the row does not have.
  // Requiring the credential is also what the switch means: a stored bot token
  // is not an OAuth client pair, so the old blob could not serve the new mode
  // even if it were kept.
  const canSave =
    changed &&
    name.trim() !== "" &&
    (!typedCredentials || credentialsComplete(mode, values)) &&
    (!modeChanged || credentialsComplete(mode, values));

  async function save() {
    setBusy(true);
    setError(undefined);
    setNotice(undefined);
    // A token credential is only usable once it has been checked: the check is
    // what writes the `auth` column, and that column is what `authenticated`
    // — and therefore hosting and `available-tools` — is computed from. So a
    // token save runs it, rather than leaving the integration saved-but-dead
    // behind a second button nobody knows to press.
    const checking = mode.kind === "token" && needsCredentials && credentialsComplete(mode, values);
    try {
      const credentials = credentialsToSave(provider, mode, values);
      await api.put<Integration>(`/integrations/${item.id}`, {
        name: name.trim(),
        type: item.type,
        enabled,
        // Spread, never `credentials: undefined` — `JSON.stringify` drops an
        // undefined value, but spelling it this way is what makes "send no
        // key" the visible intent rather than a serializer side effect.
        ...(credentials ? { credentials } : {}),
        services,
      });
      setValues({});
      // Collapse to `••• stored` only when this save actually stored one.
      // `IntegrationDetail` is keyed on the integration id, so `onChanged()`
      // does not remount it and the `useState` seed below never re-runs — an
      // unconditional `false` would leave a row with nothing stored showing
      // the masked line, claiming a secret that is not there and hiding the
      // one field that would fix it. `item` is still the pre-reload prop,
      // which is exactly right on the path where no credential was sent.
      setReplacing(credentials ? false : !item.has_credentials);
      if (checking) {
        // **After the PUT, never before**: the check reads the credentials the
        // server has stored, not the ones typed above.
        await api.post<{ valid: boolean; validated: boolean }>(
          `/integrations/${item.id}/auth/validate`
        );
      }
      setNotice(checking ? "Saved and checked against the provider." : "Saved.");
    } catch (err) {
      // The PUT may already have committed, so this never claims the save
      // failed — the new credential *is* stored, and saying otherwise would
      // send the user looking for a write that happened.
      //
      // **A rejected check does not make the badge honest**, and the copy has
      // to say so. `update` preserves a non-empty `auth` in SQL and
      // `authenticated` is computed from that column, so a row that was already
      // connected still reads `Connected` after its credential is replaced with
      // a rejected one — and `reload_blocking` has restarted its MCP server on
      // the new, bad token. The status the user can see is about the *previous*
      // authorisation; only this sentence is about the credential they just
      // saved.
      setError(
        checking
          ? `${describeError(err)} — the new credential was saved but not accepted, so any earlier authorisation is what the status above still reflects.`
          : describeError(err)
      );
    } finally {
      // Unconditional, and outside the `try`: the row changed even when the
      // check refused it, so what the pane shows has to be re-read either way.
      onChanged();
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
    // Required now that the mode is part of `changed`: without it, Revert can
    // never clear the savebar for a switched mode — and it would claim to have
    // restored the stored row while leaving the auth actions disabled.
    setModeValue(modeFor(provider, item.auth_mode).value);
    setServices(item.services ?? {});
    setValues({});
    setReplacing(!item.has_credentials);
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
          {/* The shared body first, and the edit-only sections after it
              (#517): this screen and the connect screen open on the same
              block, so the layout learned on the way in is the one met on the
              way back. */}
          <ConnectionFields
            provider={provider}
            name={name}
            onName={setName}
            mode={mode}
            modeValue={modeValue}
            onMode={setModeValue}
            values={values}
            onValues={setValues}
            /* The auth method is part of the credential blob, so it is offered
               only alongside the fields that carry it: changing it on its own
               would send nothing and silently do nothing. That is also why
               `canSave` demands a complete credential whenever the mode
               changed — see the note on it. */
            stored={replacing ? undefined : { onReplace: () => setReplacing(true) }}
          />

          <div className="divider" />

          <ServicesSection provider={provider} services={services} onChange={setServices} />

          <div className="divider" />

          <AuthSection
            item={item}
            mode={mode}
            storedMode={storedMode}
            onChanged={onChanged}
          />

          {provider.supportsTriggers && (
            <>
              <div className="divider" />
              <TriggerRules integrationId={item.id} />
              <div className="divider" />
              <WebhookPanel integrationId={item.id} />
            </>
          )}

          <div className="divider" />

          {/* Edit-only, and the last thing on the screen for that reason: a
              row has to exist before it can be turned off, and the connect
              screen deliberately offers no such switch — see `create()`. */}
          <div className="formsec">
            <div className="formsec__title">{AVAILABILITY_TITLE}</div>
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
            <SaveBar
              creating={false}
              busy={busy}
              canSubmit={canSave}
              message={
                canSave
                  ? "You have unsaved changes."
                  : name.trim() === ""
                    ? "A name is required."
                    : // Ahead of the two below, because for a switched mode
                      // "clear them to keep the stored ones" is exactly the
                      // thing that cannot work — the stored blob belongs to the
                      // other method.
                      modeChanged
                      ? `Enter the credentials for ${mode.label} — switching the auth method replaces the stored ones.`
                      : item.has_credentials
                        ? "Fill in every credential field, or clear them to keep the stored ones."
                        : "Fill in every credential field."
              }
              onDiscard={revert}
              onSubmit={save}
            />
          )}
        </div>
      </div>
    </>
  );
}

/* --- Auth ---------------------------------------------------------------- */

function AuthSection({
  item,
  mode,
  storedMode,
  onChanged,
}: {
  item: Integration;
  /** The mode the editor is currently on, **not** the provider's mode list. */
  mode: AuthMode;
  /**
   * The mode the *row* records, for telling an unsaved switch from a saved one.
   * `undefined` when nothing in the provider's list matches what the row stored:
   * a multi-mode row written before `auth_mode` reached the wire, and a GitHub
   * row that recorded no `pat`. (The four providers whose single mode is `""`
   * *do* match, since a row recording none reads back `""` — they simply cannot
   * reach the gate, having no Auth method control to switch.)
   */
  storedMode?: AuthMode;
  onChanged(): void;
}) {
  // Per selected mode, because a provider can offer both (#513). It was
  // `provider.modes.some((m) => m.kind === "oauth")`, and Slack is the only
  // provider offering an OAuth mode *and* a token mode — so `.some` was always
  // true there and the credential check below was unreachable for the one
  // provider that needs it.
  const isOAuth = mode.kind === "oauth";
  // The section follows the Auth method control, which is unsaved; both actions
  // read the *stored* credentials. When those disagree, neither action is asking
  // a question the answer would be about.
  const modeIsUnsaved = storedMode !== undefined && storedMode.value !== mode.value;

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
      // `validated` is a hardcoded list of three types on the server
      // (`token_validate.rs`'s `REPORTS_VALIDATED`) that omits github and slack
      // even though both really do call their provider — so it says nothing
      // about whether a check happened, and the old copy here ("this provider
      // has no live check") was simply untrue for the two it omits. The 200 is
      // what carries the meaning: the credentials were accepted.
      setNotice(res.validated ? "Credentials checked and accepted." : "Credentials accepted.");
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
              <button
                className="btn"
                onClick={startOAuth}
                disabled={busy || waiting || modeIsUnsaved}
              >
                <Icon name="external" size={13} />
                {item.authenticated ? "Re-authorise" : "Authorise"}
              </button>
            ) : (
              <button className="btn" onClick={validate} disabled={busy || modeIsUnsaved}>
                <Icon name="check" size={13} />
                {busy ? "Checking…" : "Validate credentials"}
              </button>
            )}
          </div>

          {/* Both actions read what is *stored*, while this section follows the
              Auth method control, which is unsaved state. One click on the other
              tab of a connected Slack row would otherwise check an OAuth blob
              with the bot-token action — an empty bearer, answered `not_authed`
              — or start an OAuth flow against credentials the row does not
              hold. Neither writes anything, so this is a confusing answer
              rather than data loss; it is still the wrong question to ask. */}
          {modeIsUnsaved ? (
            <div className="formrow__help">
              Save this auth method first — both actions use the credentials stored on the
              server, which are still{" "}
              {/* `?.` is for the type-checker only: `modeIsUnsaved` is false
                  whenever `storedMode` is undefined, so the arm cannot render. */}
              {storedMode?.label ?? "the previous method"}.
            </div>
          ) : isOAuth ? (
            <div className="formrow__help">
              Authorisation opens in your normal browser, not inside this window.
            </div>
          ) : null}
          {!isOAuth && !modeIsUnsaved && (
            <div className="formrow__help">
              {/* "above" since #517 moved this section below the connection
                  block — the sentence points at the credential fields, so it
                  has to follow them rather than describe where they used to
                  be. */}
              Checks the credentials already saved on the server, not any unsaved edits
              above.
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
        {/* The row's own mode, and `find` rather than `modeFor` for the same
            reason the gate uses it: this is a *report*, so where the editor may
            fall back to a first mode in order to render some fields, this must
            not — `modeFor` would label a legacy Slack row (`auth_mode: ""`)
            "Bot token" whatever it actually uses, which is the exact wrong
            label #513 removed from this row. The em-dash is the honest answer
            for a row that records nothing. */}
        <InspRow label="Auth">
          {provider?.modes.find((m) => m.value === item.auth_mode)?.label ?? "—"}
        </InspRow>
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
