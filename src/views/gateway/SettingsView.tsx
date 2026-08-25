import { useEffect, useState } from "react";
import { api } from "../../lib/api";
import { FormRow, Splitter, Switch } from "../../components/ui";
import { Icon } from "../../lib/icons";
import { describeError, usePoll, useResource } from "../../lib/hooks";
import type { GatewaySettings, GatewayStatus } from "../../lib/types";
import "../../styles/gateway.css";

/* ============================================================================
   LLM Gateway → Settings (#427).

   Three fields, one of which can fail after the save succeeds.

   `PUT /api/gateway/settings` stores the row and then spawns the listener
   reload without awaiting it — a save that blocked on a socket bind would feel
   like a hang — so a `200` means "stored", never "listening". The status strip
   below the form is therefore not decoration: it is the only place the outcome
   of a port change actually shows up, and it is polled rather than read once.

   The port rules are the server's, restated here so a bad value is refused
   before the round trip rather than after it: `validate` refuses 0, and the
   route refuses anything below 1024 because a lower port needs root. The
   client check is a convenience, not the authority — the server's 422 is still
   rendered if one gets past it.
   ========================================================================== */

/** The route's own floor. Below this a bind needs root on Unix. */
const MIN_PORT = 1024;
const MAX_PORT = 65535;

/**
 * The server's ceiling on the retention horizon (`MAX_RETENTION_DAYS`).
 *
 * A typo bound rather than a correctness one — `9000000` reads as "keep
 * everything" while meaning something nobody can check, and `0` is the
 * supported way to say that.
 */
const MAX_RETENTION_DAYS = 3650;

export function GatewaySettingsView({ inspectorOpen }: { inspectorOpen: boolean }) {
  const settings = useResource<GatewaySettings>(
    (signal) => api.get("/gateway/settings", signal),
    []
  );
  const status = useResource<GatewayStatus>(
    (signal) => api.get("/gateway/status", signal),
    []
  );
  usePoll(status.reload, 4_000);

  const [enabled, setEnabled] = useState(false);
  const [startWithApp, setStartWithApp] = useState(true);
  const [port, setPort] = useState("");
  const [retention, setRetention] = useState("");
  const [loaded, setLoaded] = useState(false);

  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [notice, setNotice] = useState<string>();

  // Seed the form from the stored row once, so a poll-driven re-render never
  // overwrites what the user is typing.
  useEffect(() => {
    if (loaded || !settings.data) return;
    setEnabled(settings.data.enabled);
    setStartWithApp(settings.data.start_with_app);
    setPort(String(settings.data.port));
    setRetention(String(settings.data.usage_retention_days));
    setLoaded(true);
  }, [settings.data, loaded]);

  const portNumber = Number(port);
  const portValid =
    /^\d+$/.test(port.trim()) && portNumber >= MIN_PORT && portNumber <= MAX_PORT;

  const retentionNumber = Number(retention);
  const retentionValid =
    /^\d+$/.test(retention.trim()) && retentionNumber <= MAX_RETENTION_DAYS;

  const changed =
    !!settings.data &&
    (enabled !== settings.data.enabled ||
      startWithApp !== settings.data.start_with_app ||
      portNumber !== settings.data.port ||
      retentionNumber !== settings.data.usage_retention_days);

  const portChanged = !!settings.data && portNumber !== settings.data.port;

  // Shortening the horizon deletes rows on the next prune, and the prune is the
  // only delete on that table — so it is the one change on this form that
  // changing the value back does not undo. `0` is *keep everything*, so it is
  // the longest horizon rather than the shortest: moving off it always
  // shortens, and moving onto it never does.
  const retentionShortened = (() => {
    if (!settings.data || !retentionValid) return false;
    const stored = settings.data.usage_retention_days;
    if (retentionNumber === 0) return false;
    return stored === 0 || retentionNumber < stored;
  })();

  const canSave = changed && portValid && retentionValid;

  async function save() {
    setBusy(true);
    setError(undefined);
    try {
      // Every key every time. Three of the four have no serde default, so an
      // omitted one is a 400 rather than "leave it alone"; `usage_retention_days`
      // *does* have one — for the sake of clients written against the pre-#428
      // shape — which makes sending it the difference between preserving the
      // stored horizon and silently resetting it to 90.
      const saved = await api.put<GatewaySettings>("/gateway/settings", {
        enabled,
        port: portNumber,
        start_with_app: startWithApp,
        usage_retention_days: retentionNumber,
      });
      setNotice(
        saved.enabled
          ? "Saved. The listener is restarting — watch the status below."
          : "Saved. The gateway is switched off."
      );
      settings.reload();
      status.reload();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="panes">
      <div className="pane-detail">
        <div className="toolbar">
          <div className="toolbar__title">Gateway Settings</div>
          <div className="spacer" />
        </div>

        <div className="scroll" style={{ flex: 1, padding: "var(--sp-8)" }}>
          <div className="form">
            {settings.error && (
              <div className="msgline msgline--error">
                <Icon name="alert" size={13} className="msgline__icon" />
                <span>{settings.error}</span>
              </div>
            )}
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
              <div className="formsec__title">Listener</div>

              <FormRow
                label="Enable the gateway"
                help="When off, no port is bound and the feature costs nothing."
              >
                <Switch on={enabled} onChange={setEnabled} />
              </FormRow>

              <FormRow
                label="Port"
                help={`On 127.0.0.1 only. ${MIN_PORT}–${MAX_PORT}; lower ports need root, and the OS-assigned 0 is refused because this number is what you paste into tool configs.`}
              >
                <input
                  className="field field--sm tnum"
                  type="number"
                  min={MIN_PORT}
                  max={MAX_PORT}
                  value={port}
                  onChange={(e) => setPort(e.target.value)}
                />
              </FormRow>

              <FormRow
                label="Start with the app"
                help="Bind the port at launch. Only takes effect while the gateway is enabled."
              >
                <Switch on={startWithApp} onChange={setStartWithApp} />
              </FormRow>

              {!portValid && port !== "" && (
                <div className="msgline msgline--error">
                  <Icon name="alert" size={13} className="msgline__icon" />
                  <span>
                    The port must be a whole number between {MIN_PORT} and{" "}
                    {MAX_PORT}.
                  </span>
                </div>
              )}

              {portChanged && portValid && enabled && (
                <div className="msgline msgline--warn">
                  <Icon name="alert" size={13} className="msgline__icon" />
                  <span>
                    Saving restarts the listener on the new port. Anything
                    already configured against the old one stops working until
                    you update it.
                  </span>
                </div>
              )}
            </div>

            <div className="divider" />

            <div className="formsec">
              <div className="formsec__title">Usage log</div>

              <FormRow
                label="Keep usage rows for"
                help={`Days. 0 keeps everything. One row is written per served request, and the prune that enforces this is the only thing that ever deletes them. Up to ${MAX_RETENTION_DAYS}.`}
              >
                <input
                  className="field field--sm tnum"
                  type="number"
                  min={0}
                  max={MAX_RETENTION_DAYS}
                  value={retention}
                  onChange={(e) => setRetention(e.target.value)}
                />
              </FormRow>

              {/* No `retention !== ""` guard, unlike the port field above: an
                  emptied box is exactly the state that needs the message. With a
                  stored 90 the savebar says "Fix the retention horizon to save."
                  and nothing was highlighted; with a stored 0, `Number("")` is 0
                  so `changed` is false and there was no feedback at all. */}
              {!retentionValid && (
                <div className="msgline msgline--error">
                  <Icon name="alert" size={13} className="msgline__icon" />
                  <span>
                    Keep usage rows for a whole number of days, up to{" "}
                    {MAX_RETENTION_DAYS}. Use 0 to keep everything.
                  </span>
                </div>
              )}

              {retentionShortened && (
                <div className="msgline msgline--warn">
                  <Icon name="alert" size={13} className="msgline__icon" />
                  <span>
                    Shortening the horizon deletes anything already older than
                    it, the next time the log is pruned. Usage rows are not
                    recoverable, and the Usage view will report the shortened
                    window as a floor.
                  </span>
                </div>
              )}
            </div>

            <div className="divider" />

            <div className="formsec">
              <div className="formsec__title">Current state</div>
              <div className="gw-statusline">
                <span className={`dot ${dotFor(status.data)}`} />
                <span>{sentenceFor(status.data)}</span>
              </div>
              {status.data?.error && (
                <code className="gw-status__error mono">
                  {status.data.error}
                </code>
              )}
            </div>
          </div>
        </div>

        {changed && (
          <div className="savebar">
            <span className="savebar__text">
              {canSave
                ? "You have unsaved changes."
                : portValid
                  ? "Fix the retention horizon to save."
                  : "Fix the port to save."}
            </span>
            <button
              className="btn"
              disabled={busy}
              onClick={() => {
                if (!settings.data) return;
                setEnabled(settings.data.enabled);
                setStartWithApp(settings.data.start_with_app);
                setPort(String(settings.data.port));
                setRetention(String(settings.data.usage_retention_days));
              }}
            >
              Revert
            </button>
            <button
              className="btn btn--primary"
              onClick={save}
              disabled={!canSave || busy}
            >
              {busy ? "Saving…" : "Save"}
            </button>
          </div>
        )}
      </div>

      {inspectorOpen && (
        <>
          <Splitter variable="--inspector-w" min={220} max={420} invert />
          <aside className="pane-inspector">
            <div className="inspector__head">Gateway Settings</div>
            <div className="inspector__scroll scroll">
              <div className="insp-group">
                <div className="insp-group__title">Why a fixed port</div>
                <p className="gw-help">
                  The port is what you paste into a tool's configuration, so it
                  has to be the number you chose and it has to survive a
                  restart. Port 0 — "let the OS pick" — is refused for that
                  reason rather than as an oversight.
                </p>
              </div>
            </div>
          </aside>
        </>
      )}
    </div>
  );
}

function dotFor(status: GatewayStatus | undefined): string {
  switch (status?.state) {
    case "running":
      return "dot--green";
    case "bind_failed":
    case "start_failed":
      return "dot--red";
    case "stopped":
      return "dot--idle";
    default:
      return "dot--amber";
  }
}

function sentenceFor(status: GatewayStatus | undefined): string {
  switch (status?.state) {
    case "running":
      return `Listening on 127.0.0.1:${status.port}.`;
    case "bind_failed":
      return `Port ${status.port} is already in use by another process. Choose a different one above.`;
    case "start_failed":
      return "The gateway could not be built from the configured providers, so no port was bound.";
    case "stopped":
      return "Not listening.";
    default:
      return "Reading the listener's state…";
  }
}
