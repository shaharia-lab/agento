import { useCallback, useState } from "react";
import { api } from "../../lib/api";
import { CopyButton } from "../../components/CopyButton";
import { TokenReveal } from "../../components/TokenReveal";
import { describeError, useResource } from "../../lib/hooks";
import { dateTime, relativeTime } from "../../lib/format";
import { useHostInfo } from "../../lib/host";
import type { ApiTokenRow, CreatedApiToken, ScheduledTask } from "../../lib/types";
import "../../styles/hooktokens.css";

/* ============================================================================
   A saved task's Webhook group (#697): its hook tokens, and the `curl` that
   starts it through `POST /api/hooks/{task_id}`.

   A hook token is minted through the same `POST /api/security/tokens` as every
   other credential, with `scope: "hook"` and this task's id, so listing and
   revoking stay under the `write`-gated `/api/security/*` prefix and the
   Security tab shows the same rows. The token itself is held in component
   state for the reveal and nowhere else (`TokenReveal`'s rules).
   ========================================================================== */

/** What the `curl` puts where the token goes once the reveal is dismissed. */
export const HOOK_TOKEN_VAR = "$AGENTO_HOOK_TOKEN";

/** A POSIX shell single-quoted word: `'` becomes `'\''`. */
function shellQuote(s: string): string {
  return `'${s.replace(/'/g, `'\\''`)}'`;
}

/**
 * The `curl` that starts `taskId` through its webhook.
 *
 * The port is read from `apiPortFile` at the moment the command runs, because a
 * release build binds a new OS-chosen port on every launch and a literal one
 * would stop working at the next restart. With no port file (a page outside
 * the app), `fallbackBase` is used as a literal origin instead.
 *
 * Without a `token`, the command reads `$AGENTO_HOOK_TOKEN`, so it stays
 * copy-pasteable once the token has been exported.
 */
export function hookCurl(
  apiPortFile: string,
  taskId: string,
  token?: string,
  fallbackBase = "http://127.0.0.1:8990"
): string {
  const base = apiPortFile
    ? `http://127.0.0.1:$(cat ${shellQuote(apiPortFile)})`
    : fallbackBase;
  return [
    `curl -sS -X POST "${base}/api/hooks/${taskId}"`,
    `  -H 'Content-Type: application/json'`,
    `  -H "Authorization: Bearer ${token ?? HOOK_TOKEN_VAR}"`,
    `  -d '{}'`,
  ].join(" \\\n");
}

export function HookTokens({ task }: { task: ScheduledTask }) {
  const host = useHostInfo();
  const tokens = useResource<ApiTokenRow[] | null>(
    (signal) => api.get<ApiTokenRow[] | null>("/security/tokens", signal),
    []
  );
  const rows = (tokens.data ?? []).filter((t) => t.task_id === task.id);

  const [name, setName] = useState("");
  const [days, setDays] = useState("90");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [created, setCreated] = useState<CreatedApiToken>();
  const [confirmRevoke, setConfirmRevoke] = useState<string>();

  const run = useCallback(async (fn: () => Promise<void>) => {
    setBusy(true);
    setError(undefined);
    try {
      await fn();
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }, []);

  const portFile = host?.api_port_file ?? "";
  const fallback = host?.api_base || window.location.origin;
  const curl = hookCurl(portFile, task.id, created?.token, fallback);

  return (
    <div className="hooktokens">
      {created ? (
        <TokenReveal
          name={created.name}
          token={created.token}
          onDismiss={() => setCreated(undefined)}
        >
          <CurlBlock curl={curl} />
          <p className="hooktokens__note">
            Once this banner is dismissed the command reads the token from{" "}
            <code className="mono">{HOOK_TOKEN_VAR}</code>.
          </p>
        </TokenReveal>
      ) : (
        <CurlBlock curl={curl} />
      )}

      {task.status === "paused" && (
        <p className="hooktokens__note">Calls answer 409 while the task is paused.</p>
      )}
      {host?.os === "windows" && (
        <p className="hooktokens__note">
          The command needs a POSIX shell, such as Git Bash or WSL.
        </p>
      )}

      {tokens.error && !tokens.data ? (
        <p className="hooktokens__error">{tokens.error}</p>
      ) : tokens.loading && !tokens.data ? (
        <p className="hooktokens__note">Reading hook tokens.</p>
      ) : rows.length === 0 ? (
        <p className="hooktokens__note">No hook tokens for this task yet.</p>
      ) : (
        <ul className="hooktokens__list">
          {rows.map((t) => (
            <li
              key={t.id}
              className={`hooktokens__item ${t.revoked_at ? "hooktokens__item--revoked" : ""}`}
            >
              <div className="row">
                <span className="truncate hooktokens__name" title={t.name}>
                  {t.name}
                </span>
                {t.revoked_at ? (
                  <span className="badge" title={dateTime(t.revoked_at)}>
                    revoked
                  </span>
                ) : confirmRevoke !== t.id ? (
                  <button className="btn btn--ghost" onClick={() => setConfirmRevoke(t.id)}>
                    Revoke
                  </button>
                ) : null}
              </div>
              <div className="hooktokens__meta">
                <span title={dateTime(t.created_at)}>created {relativeTime(t.created_at)}</span>
                <span title={t.last_used_at ? dateTime(t.last_used_at) : undefined}>
                  used {t.last_used_at ? relativeTime(t.last_used_at) : "never"}
                </span>
                <span title={t.expires_at ? dateTime(t.expires_at) : undefined}>
                  expires {t.expires_at ? relativeTime(t.expires_at) : "never"}
                </span>
              </div>
              {confirmRevoke === t.id && !t.revoked_at && (
                <div className="hooktokens__confirm">
                  <span>Revoke {t.name}? Anything using it stops working.</span>
                  <div className="row">
                    <button
                      className="btn btn--ghost"
                      onClick={() => setConfirmRevoke(undefined)}
                    >
                      Cancel
                    </button>
                    <button
                      className="btn btn--danger"
                      disabled={busy}
                      onClick={() =>
                        run(async () => {
                          await api.del(`/security/tokens/${t.id}`);
                          setConfirmRevoke(undefined);
                          tokens.reload();
                        })
                      }
                    >
                      Revoke
                    </button>
                  </div>
                </div>
              )}
            </li>
          ))}
        </ul>
      )}

      <div className="hooktokens__form">
        <div className="hooktokens__label">New hook token</div>
        <input
          className="field"
          value={name}
          placeholder="Name, e.g. post-commit hook"
          onChange={(e) => setName(e.target.value)}
        />
        <label className="row hooktokens__days">
          <span>Expires in</span>
          <input
            className="field field--sm tnum"
            type="number"
            min={1}
            max={3650}
            value={days}
            onChange={(e) => setDays(e.target.value)}
          />
          <span>days</span>
        </label>
        <div className="row">
          <button
            className="btn btn--primary"
            disabled={busy || !name.trim()}
            onClick={() =>
              run(async () => {
                const token = await api.post<CreatedApiToken>("/security/tokens", {
                  name: name.trim(),
                  scope: "hook",
                  task_id: task.id,
                  expires_in_days: Number(days) || 0,
                });
                setCreated(token);
                setName("");
                tokens.reload();
              })
            }
          >
            Create
          </button>
          {!name.trim() && <span className="hooktokens__note">Name the token first.</span>}
        </div>
        {error && <p className="hooktokens__error">{error}</p>}
      </div>
    </div>
  );
}

/** The command, wrapped, with its copy button. */
function CurlBlock({ curl }: { curl: string }) {
  return (
    <div className="hooktokens__curl">
      <pre className="mono">{curl}</pre>
      <CopyButton text={curl} title="Copy command" className="btn" label="Copy" />
    </div>
  );
}
