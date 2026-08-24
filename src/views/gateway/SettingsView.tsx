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
    setLoaded(true);
  }, [settings.data, loaded]);

  const portNumber = Number(port);
  const portValid =
    /^\d+$/.test(port.trim()) && portNumber >= MIN_PORT && portNumber <= MAX_PORT;

  const changed =
    !!settings.data &&
    (enabled !== settings.data.enabled ||
      startWithApp !== settings.data.start_with_app ||
      portNumber !== settings.data.port);

  const portChanged = !!settings.data && portNumber !== settings.data.port;
  const canSave = changed && portValid;

  async function save() {
    setBusy(true);
    setError(undefined);
    try {
      // All three keys every time: the request struct has no serde defaults, so
      // an omitted field is a 400 rather than "leave it alone".
      const saved = await api.put<GatewaySettings>("/gateway/settings", {
        enabled,
        port: portNumber,
        start_with_app: startWithApp,
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
              <div className="formsec__title">Current state</div>
              <div className="gw-statusline">
                <span className={`dot ${dotFor(status.data)}`} />
                <span>{sentenceFor(status.data)}</span>
              </div>
              {status.data?.error && (
                <code className="gw-status__error mono selectable">
                  {status.data.error}
                </code>
              )}
            </div>
          </div>
        </div>

        {changed && (
          <div className="savebar">
            <span className="savebar__text">
              {canSave ? "You have unsaved changes." : "Fix the port to save."}
            </span>
            <button
              className="btn"
              disabled={busy}
              onClick={() => {
                if (!settings.data) return;
                setEnabled(settings.data.enabled);
                setStartWithApp(settings.data.start_with_app);
                setPort(String(settings.data.port));
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
