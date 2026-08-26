import { useCallback, useState } from "react";
import { api } from "../../lib/api";
import { CopyButton } from "../../components/CopyButton";
import { TokenReveal } from "../../components/TokenReveal";
import {
  Empty,
  FormRow,
  InspGroup,
  InspRow,
  Segmented,
  Splitter,
} from "../../components/ui";
import { Icon } from "../../lib/icons";
import { describeError, usePoll, useResource } from "../../lib/hooks";
import type { ViewId } from "../../lib/nav";
import type {
  CreatedApiToken,
  GatewaySettings,
  GatewayStatus,
} from "../../lib/types";
import {
  anthropicBaseUrl,
  effectivePort,
  healthUrl,
  openaiBaseUrl,
  snippetsFor,
  TOKEN_PLACEHOLDER,
} from "./snippets";
import "../../styles/gateway.css";

/* ============================================================================
   LLM Gateway → Overview (#427).

   Two jobs, and the second is the one the feature lives or dies on:

   1. Say what the listener is doing — and when it is *not* doing it, say why in
      a way the user can act on. A bind failure names the port and routes to the
      setting that changes it, because that is the only lever they have.
   2. Get a working tool configuration onto the clipboard in one click. The
      distance between "the gateway is running" and "my tool is pointed at it"
      is the whole adoption question, so the token is minted here rather than
      sending the user to Settings → Security to do it by hand.

   The token that mint produces exists in this component's state and nowhere
   else. It is never written to localStorage, never lifted into a context, and
   never re-fetched — `GET /api/security/tokens` answers rows without a `token`
   field, so there is nothing to re-render it from. Navigating away loses it,
   which is the #405 invariant working rather than a gap.
   ========================================================================== */

/** How the four states read, and what colour they carry. */
const STATE_COPY: Record<
  GatewayStatus["state"],
  { label: string; dot: string; badge: string }
> = {
  running: { label: "Running", dot: "dot--green", badge: "badge--green" },
  stopped: { label: "Stopped", dot: "dot--idle", badge: "badge" },
  bind_failed: { label: "Port unavailable", dot: "dot--red", badge: "badge--red" },
  start_failed: { label: "Failed to start", dot: "dot--red", badge: "badge--red" },
};

const UNKNOWN_STATE = {
  label: "Unknown",
  dot: "dot--amber",
  badge: "badge--amber",
};

export function GatewayOverviewView({
  inspectorOpen,
  onNavigate,
}: {
  inspectorOpen: boolean;
  onNavigate(view: ViewId): void;
}) {
  const status = useResource<GatewayStatus>(
    (signal) => api.get("/gateway/status", signal),
    []
  );
  const settings = useResource<GatewaySettings>(
    (signal) => api.get("/gateway/settings", signal),
    []
  );

  // A write answers before the listener has restarted — the reload is spawned,
  // not awaited — so polling is the only truth about the port. Slower when the
  // gateway is off, because then nothing is expected to change on its own.
  const running = status.data?.state === "running";
  usePoll(status.reload, running ? 10_000 : 4_000);

  const [created, setCreated] = useState<CreatedApiToken>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  // Which snippet is on show. `curl` unconditionally, because "does it answer
  // at all" is the first thing to try — even though it cannot succeed until a
  // token exists, which is what the amber banner above the tabs already says.
  // Deliberately not persisted: one view, one visit, no state worth carrying.
  const [tab, setTab] = useState("curl");

  const mint = useCallback(async () => {
    setBusy(true);
    setError(undefined);
    try {
      // `llm` is disjoint from `read`/`write` (#423): this token spends provider
      // credits and can reach nothing on `/api`. The app's own session is
      // `write`-scoped, so it may mint one and can never use one.
      const token = await api.post<CreatedApiToken>("/security/tokens", {
        name: "LLM gateway client",
        scope: "llm",
        expires_in_days: 365,
      });
      setCreated(token);
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }, []);

  const state = status.data?.state;
  const copy = (state && STATE_COPY[state]) || UNKNOWN_STATE;
  const port = effectivePort(status.data, settings.data?.port ?? 0);
  const snippets = port > 0 ? snippetsFor(port, created?.token) : [];
  // `tab` is a key rather than an index, so it survives the list being rebuilt
  // on every mint. The fallback covers a key that is no longer in the list; it
  // is never read when `snippets` is empty, which the `Empty` branch guards.
  const active = snippets.find((s) => s.key === tab) ?? snippets[0];

  return (
    <div className="panes">
      <div className="pane-detail gw-overview">
        <div className="toolbar">
          <div className="toolbar__title">Overview</div>
          <div className="toolbar__sep" />
          <span className={`dot ${copy.dot}`} />
          <span className="toolbar__sub">{copy.label}</span>
          <div className="spacer" />
          <button
            className="btn"
            onClick={() => {
              status.reload();
              settings.reload();
            }}
            title="Refresh"
          >
            <Icon name="refresh" size={13} />
            Refresh
          </button>
        </div>

        <div className="scroll" style={{ flex: 1, padding: "var(--sp-8)" }}>
          <div className="form">
            {/* Both reads are surfaced. A swallowed settings error is the
                nastier of the two: `port` would fall back to 0 and the snippets
                below would be replaced by "No port configured", which blames
                the user's configuration for a failed request. */}
            {(
              [
                ["status", status.error],
                ["settings", settings.error],
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

            {/* ── Status ──────────────────────────────────────────────────────
                Only when the listener is *not* running (#473). A running one is
                already reported three times over — the toolbar dot and label
                above, the inspector's Listener/Endpoints groups, and the port
                printed inside every snippet — so in the state users spend all
                their time in this card is pure duplication pushing the snippets
                past the fold. What it uniquely carries is the failure
                explanation, `status.error`, and the button to Gateway Settings,
                and none of those exist while it runs. */}
            {state !== "running" && (
              <>
                <div className="formsec">
                  <div className="formsec__title">Listener</div>

                  <div className={`gw-status gw-status--${state ?? "unknown"}`}>
                    <div className="gw-status__head">
                      <span className={`badge ${copy.badge}`}>{copy.label}</span>
                      {status.data?.port !== undefined && (
                        <span className="mono tnum gw-status__port">
                          127.0.0.1:{status.data.port}
                        </span>
                      )}
                    </div>

                    <p className="gw-status__text">
                      {explain(status.data, settings.data)}
                    </p>

                    {status.data?.error && (
                      <code className="gw-status__error mono">
                        {status.data.error}
                      </code>
                    )}

                    {/* The one lever a failed or stopped listener leaves the
                        user, so it is a link rather than a sentence telling
                        them to go and find it. */}
                    <div className="row">
                      <button
                        className="btn btn--primary"
                        onClick={() => onNavigate("gateway-settings")}
                      >
                        <Icon name="gear" size={13} />
                        {state === "bind_failed"
                          ? "Change the port"
                          : "Open gateway settings"}
                      </button>
                    </div>
                  </div>
                </div>

                <div className="divider" />
              </>
            )}

            {/* ── The credential ──────────────────────────────────────────── */}
            <div className="formsec">
              <div className="formsec__title">Gateway token</div>

              {created ? (
                <TokenReveal
                  name={created.name}
                  token={created.token}
                  onDismiss={() => setCreated(undefined)}
                >
                  <p className="gw-help">
                    It is embedded in the snippets below while this banner is
                    open. Leaving this view loses it — mint another if you need
                    one.
                  </p>
                </TokenReveal>
              ) : (
                <div className="msgline msgline--warn">
                  <Icon name="shield" size={13} className="msgline__icon" />
                  <span>
                    The snippets below show <code className="mono">{TOKEN_PLACEHOLDER}</code>{" "}
                    until you mint a token. A gateway token can spend your
                    provider credits and can reach nothing else in Agento.
                  </span>
                </div>
              )}

              {error && (
                <div className="msgline msgline--error">
                  <Icon name="alert" size={13} className="msgline__icon" />
                  <span>{error}</span>
                </div>
              )}

              <FormRow
                label=""
                help="Creates an llm-scoped token valid for a year. Manage and revoke tokens in Settings → Security."
              >
                <div className="row">
                  <button className="btn btn--primary" disabled={busy} onClick={mint}>
                    <Icon name="plus" size={14} />
                    {busy ? "Creating…" : "Create gateway token"}
                  </button>
                </div>
              </FormRow>
            </div>

            <div className="divider" />

            {/* ── Snippets ────────────────────────────────────────────────── */}
            <div className="formsec">
              <div className="formsec__title">Point a tool at it</div>

              {snippets.length === 0 ? (
                <Empty
                  icon="zap"
                  title={settings.error ? "Settings unavailable" : "No port configured"}
                  text={
                    settings.error
                      ? "The port could not be read, so there is nothing to build a snippet from."
                      : "Set a port in Gateway Settings and the snippets appear here."
                  }
                />
              ) : (
                /* One block, four tabs — the user reads exactly one of these,
                   so stacking all four cost ~330px of scroll to show three
                   snippets nobody was looking at. `Segmented` is the repo's tab
                   primitive and brings role="tablist"/aria-selected with it. */
                <div className="gw-snippet">
                  <div className="gw-snippet__head">
                    <Segmented
                      value={active.key}
                      options={snippets.map((s) => ({
                        value: s.key,
                        label: s.title,
                      }))}
                      onChange={setTab}
                    />
                    <div className="spacer" />
                    {/* Straight from `snippetsFor()` — the clipboard text is
                        the snippet's own bytes, never re-derived here. */}
                    <CopyButton
                      text={active.body}
                      title={`Copy the ${active.title} snippet`}
                      className="btn"
                      label="Copy"
                    />
                  </div>
                  <p className="gw-snippet__note">{active.note}</p>
                  <pre className="codebox">{active.body}</pre>
                </div>
              )}
            </div>
          </div>
        </div>
      </div>

      {inspectorOpen && (
        <>
          <Splitter variable="--inspector-w" min={220} max={420} invert />
          <aside className="pane-inspector">
            <div className="inspector__head">Gateway</div>
            <div className="inspector__scroll scroll">
              <InspGroup title="Listener">
                <InspRow label="State">{copy.label}</InspRow>
                <InspRow label="Port">
                  {status.data?.port ?? settings.data?.port ?? "—"}
                </InspRow>
                <InspRow label="Enabled">
                  {settings.data ? (settings.data.enabled ? "Yes" : "No") : "—"}
                </InspRow>
                <InspRow label="Start with app">
                  {settings.data
                    ? settings.data.start_with_app
                      ? "Yes"
                      : "No"
                    : "—"}
                </InspRow>
              </InspGroup>
              {/* Each row abbreviates the URL to fit the pane but copies the
                  whole thing: the base URLs are the one value in this feature a
                  user retypes by hand, and one wrong character (`/anthropic/v1`)
                  is the documented failure. See snippets.ts. */}
              <InspGroup title="Endpoints">
                <InspRow label="OpenAI">
                  {port > 0 ? (
                    <span className="row insp-row__copy">
                      <span className="truncate">{`:${port}/v1`}</span>
                      <CopyButton
                        text={openaiBaseUrl(port)}
                        title="Copy the OpenAI base URL"
                      />
                    </span>
                  ) : (
                    "—"
                  )}
                </InspRow>
                <InspRow label="Anthropic">
                  {port > 0 ? (
                    <span className="row insp-row__copy">
                      <span className="truncate">{`:${port}/anthropic`}</span>
                      <CopyButton
                        text={anthropicBaseUrl(port)}
                        title="Copy the Anthropic base URL"
                      />
                    </span>
                  ) : (
                    "—"
                  )}
                </InspRow>
                <InspRow label="Health">
                  {port > 0 ? (
                    <span className="row insp-row__copy">
                      <span className="truncate">{healthUrl(port)}</span>
                      <CopyButton
                        text={healthUrl(port)}
                        title="Copy the health-check URL"
                      />
                    </span>
                  ) : (
                    "—"
                  )}
                </InspRow>
              </InspGroup>
            </div>
          </aside>
        </>
      )}
    </div>
  );
}

/** One sentence per state, saying what happened and what to do about it. */
function explain(
  status: GatewayStatus | undefined,
  settings: GatewaySettings | undefined
): string {
  switch (status?.state) {
    case "running":
      return "The gateway is accepting requests on the loopback address. Only this machine can reach it.";
    case "bind_failed":
      return `Another process is already listening on port ${status.port}. Pick a different port and save — nothing else needs to change.`;
    case "start_failed":
      return "The gateway could not be built from the configured providers, so no port was bound. Check the provider rows below the error.";
    case "stopped":
      return settings?.enabled === false
        ? "The gateway is switched off. Enable it in Gateway Settings to bind the port."
        : "The gateway is not listening.";
    default:
      return "Reading the listener's state…";
  }
}
