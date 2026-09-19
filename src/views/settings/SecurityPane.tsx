import { useCallback, useState } from "react";
import { api } from "../../lib/api";
import { CopyButton } from "../../components/CopyButton";
import { TokenReveal } from "../../components/TokenReveal";
import { Dropdown, Empty, FormRow, Switch } from "../../components/ui";
import { Icon } from "../../lib/icons";
import { describeError, useResource } from "../../lib/hooks";
import { dateTime, relativeTime, tildePath } from "../../lib/format";
import { useHostInfo } from "../../lib/host";
import type {
  ApiTokenRow,
  CreatedApiToken,
  CredentialWhitelistEntry,
} from "../../lib/types";
import "../../styles/security.css";

/* ============================================================================
   Settings → Security (#405).

   What this pane manages is the credential every `/api` request carries: an
   EdDSA (Ed25519) JWT signed by a keypair generated on this install's first run.
   It shows the **public** half, and lets the user issue, inspect and revoke
   tokens for other local processes.

   Three things it deliberately does not do, each for a reason worth keeping:

   - **It never shows the private key**, and no route would return it. That is
     `CLAUDE.md`'s standing rule about stored secrets, and it applies harder to a
     durable signing key than to anything it was written for: rendering it would
     put the key in an `/api` response body, in this webview's DOM, and in any
     screenshot of this page. Nothing needs it — what another service consumes is
     the public key, over JWKS.
   - **It never re-shows a created token.** Nothing stores it, so this is not a
     policy that could be relaxed: the response to the creation request is the
     only copy that will ever exist.
   - **It uses no `window.confirm`.** That wedges the WebView (see `CLAUDE.md`),
     so revoke and regenerate are inline two-step confirmations like the profile
     deletes in the parent view.

   The Credentials Checker (#606) is a section of this pane, not a second
   "Security" tab. Its toggle is a field of `/api/settings`, so it is held in
   the parent's settings draft and saved by the parent's `SaveBar` like every
   other setting; the whitelist is its own resource and writes immediately.
   ========================================================================== */

interface KeyInfo {
  kid: string;
  public_key: string;
  algorithm: string;
  jwks_path: string;
  private_key_path: string;
  public_key_path: string;
}

/* The two token shapes moved to `lib/types.ts` when the LLM Gateway Overview
   (#427) became a second consumer of `POST /api/security/tokens`. */
type TokenRow = ApiTokenRow;
type CreatedToken = CreatedApiToken;

const SCOPES = [
  { value: "read", label: "Read only" },
  { value: "write", label: "Read and write" },
  { value: "llm", label: "LLM gateway" },
];

/**
 * What a `write` token can actually do, stated where the choice is made.
 *
 * This is not boilerplate. `POST /api/agents` can create an agent with
 * `permission_mode: bypass` and `POST /api/chats/{id}/messages` can run it, so a
 * `write` token is arbitrary command execution on this machine as this user. A
 * warning buried in the docs would be a warning nobody handing one out reads.
 */
const WRITE_WARNING =
  "A write token can create an agent with bypassed permissions and run it — " +
  "that is arbitrary command execution on this machine, as you. Only issue one " +
  "to something you would trust with a shell.";

/**
 * ...and `read` is not "safe" either, which is worth saying rather than implying
 * a least privilege it does not have.
 */
const READ_WARNING =
  "A read token can read every chat transcript, agent system prompt and " +
  "integration list on this machine. It cannot change anything.";

/**
 * ...and `llm` is the one whose cost is money rather than access (#423).
 *
 * Says both halves, because both are the point: it spends real provider credits,
 * and it reaches nothing else. A gateway token is meant to be pasted into tool
 * configs where it sits in plaintext, so the honest thing to state is the actual
 * ceiling rather than "limited access".
 */
const LLM_WARNING =
  "An LLM gateway token can spend your configured LLM provider credits, with " +
  "no spending limit of its own. It cannot read or change anything in Agento.";

/**
 * Shown when a scope has no copy of its own — i.e. a value was added to
 * `SCOPES` and not to `SCOPE_WARNINGS`.
 *
 * Deliberately *not* one of the three real warnings. Each of those makes a
 * positive claim about what the scope does and does not reach, and asserting any
 * of them about a scope this file knows nothing about would be a guess shown to
 * the person deciding whether to hand the token out.
 */
const UNKNOWN_SCOPE_WARNING =
  "This build does not describe what this scope grants. Do not issue it.";

/**
 * The capability note shown under the scope picker.
 *
 * A lookup rather than a ternary: at two scopes a ternary read fine, at three it
 * would nest, and the next scope would nest it again.
 */
const SCOPE_WARNINGS: Record<string, string> = {
  read: READ_WARNING,
  write: WRITE_WARNING,
  llm: LLM_WARNING,
};

/**
 * The badge class per scope. `write` is amber because it is the dangerous one;
 * `llm` gets its own colour because three scopes reading as two visuals is how a
 * gateway token gets mistaken for a read token at a glance.
 */
const SCOPE_BADGES: Record<string, string> = {
  write: "badge badge--amber",
  llm: "badge badge--purple",
};

/**
 * The Credentials Checker toggle, as the parent's settings draft holds it.
 * `enabled` is `undefined` until `/api/settings` has loaded, and `error` says
 * why it never will — this pane renders outside the settings form's gates, so
 * it has to say so itself.
 */
export interface CheckerToggle {
  enabled: boolean | undefined;
  error?: string;
  /** The environment variable that pinned the field, if one did. */
  lock?: string;
  onChange(v: boolean): void;
}

export function SecurityPane({ checker }: { checker: CheckerToggle }) {
  const host = useHostInfo();
  const keys = useResource<KeyInfo>(
    (signal) => api.get<KeyInfo>("/security/keys", signal),
    []
  );
  const tokens = useResource<TokenRow[] | null>(
    (signal) => api.get<TokenRow[] | null>("/security/tokens", signal),
    []
  );

  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [confirmRegenerate, setConfirmRegenerate] = useState(false);
  const [confirmRevoke, setConfirmRevoke] = useState<string>();
  const [created, setCreated] = useState<CreatedToken>();
  const [name, setName] = useState("");
  const [scope, setScope] = useState("read");
  const [days, setDays] = useState("90");

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

  const base = host?.api_base ?? "";
  const jwksUrl = keys.data ? `${base}${keys.data.jwks_path}` : "";

  if (keys.loading && !keys.data) {
    return (
      <Empty icon="shield" title="Loading" text="Reading the signing key." />
    );
  }
  if (keys.error && !keys.data) {
    return (
      <Empty
        icon="alert"
        title="Security unavailable"
        text={keys.error}
        action={
          <button className="btn btn--lg" onClick={keys.reload}>
            <Icon name="refresh" size={14} />
            Retry
          </button>
        }
      />
    );
  }
  if (!keys.data) return null;

  const rows = tokens.data ?? [];

  return (
    <div className="form">
      {error && (
        <div className="msgline msgline--error">
          <Icon name="alert" size={14} className="msgline__icon" />
          <span>{error}</span>
        </div>
      )}

      {/* ── The signing key ───────────────────────────────────────────────── */}
      <div className="formsec">
        <div className="formsec__title">Signing key</div>

        <FormRow
          label="Key ID"
          help="Named in every token this install issues. It changes when the key does, so a token whose kid does not match this one was issued before the last regenerate."
        >
          <div className="row">
            <span className="mono truncate">{keys.data.kid}</span>
            <CopyButton text={keys.data.kid} title="Copy key ID" />
          </div>
        </FormRow>

        <FormRow
          label="Public key"
          help="Ed25519, base64url. This is what a verifier needs — it can check a token Agento issued and cannot mint one."
        >
          <div className="row">
            <span className="mono truncate">
              {keys.data.public_key}
            </span>
            <CopyButton text={keys.data.public_key} title="Copy public key" />
          </div>
        </FormRow>

        <FormRow
          label="JWKS"
          help="Serves the public key in the standard form, with no credential required. Point a stock JWT library at this and it can verify Agento's tokens offline."
        >
          <div className="row">
            <span className="mono truncate">{jwksUrl}</span>
            <CopyButton text={jwksUrl} title="Copy JWKS URL" />
          </div>
        </FormRow>

        <FormRow
          label="Key files"
          help="The private key is never shown here and no API route returns it. Its path is listed so you can back it up or move it aside."
        >
          <div className="col">
            <div className="row">
              <span className="mono truncate" title={keys.data.private_key_path}>
                {tildePath(keys.data.private_key_path)}
              </span>
              <span className="badge">private</span>
            </div>
            <div className="row">
              <span className="mono truncate" title={keys.data.public_key_path}>
                {tildePath(keys.data.public_key_path)}
              </span>
              <span className="badge badge--green">public</span>
            </div>
          </div>
        </FormRow>

        <FormRow
          label="Regenerate"
          help="Creates a new keypair. Every token ever issued stops working immediately. This window's recovers on its own; anything else holding one — a script, or a tool configured against the LLM gateway — starts getting 401s with no other signal and needs a new token issued by hand."
        >
          {confirmRegenerate ? (
            <div className="row">
              <span className="confirm" style={{ flex: 1 }}>
                Replace the signing key? Every issued token stops working and
                cannot be restored — including LLM gateway tokens, so any tool
                configured against the gateway stops until you issue it a new
                one.
              </span>
              <button
                className="btn btn--ghost"
                onClick={() => setConfirmRegenerate(false)}
              >
                Cancel
              </button>
              <button
                className="btn btn--danger"
                disabled={busy}
                onClick={() =>
                  run(async () => {
                    await api.post<KeyInfo>("/security/keys/regenerate");
                    setConfirmRegenerate(false);
                    setCreated(undefined);
                    keys.reload();
                    tokens.reload();
                  })
                }
              >
                Regenerate
              </button>
            </div>
          ) : (
            /* In a `.row` rather than bare: `.formrow__control` is a block, so
               a lone button stretches to the column's full width and reads as a
               field rather than an action. Every other button here is already
               inside one. */
            <div className="row">
              <button
                className="btn btn--danger"
                onClick={() => setConfirmRegenerate(true)}
              >
                <Icon name="refresh" size={14} />
                Regenerate key
              </button>
            </div>
          )}
        </FormRow>
      </div>

      <div className="divider" />

      {/* ── Issue a token ─────────────────────────────────────────────────── */}
      <div className="formsec">
        <div className="formsec__title">Issue a token</div>

        {/* Shown once, and there is no second chance — nothing stores it. The
            banner stays until dismissed for that reason: a toast that faded
            would lose the credential. */}
        {created && (
          <TokenReveal
            name={created.name}
            token={created.token}
            onDismiss={() => setCreated(undefined)}
          />
        )}

        <FormRow label="Name" help="What this token is for. Shown in the list below.">
          <input
            className="field"
            value={name}
            placeholder="e.g. release script"
            onChange={(e) => setName(e.target.value)}
          />
        </FormRow>

        <FormRow
          label="Scope"
          help={SCOPE_WARNINGS[scope] ?? UNKNOWN_SCOPE_WARNING}
        >
          <Dropdown value={scope} options={SCOPES} onChange={setScope} />
        </FormRow>

        <FormRow
          label="Expires in"
          help="Days. The token stops working then, whether or not it has been revoked."
        >
          <input
            className="field field--sm tnum"
            type="number"
            min={1}
            max={3650}
            value={days}
            onChange={(e) => setDays(e.target.value)}
          />
        </FormRow>

        <FormRow label="">
          <div className="row">
            <button
              className="btn btn--primary"
              disabled={busy || !name.trim()}
              onClick={() =>
                run(async () => {
                  const token = await api.post<CreatedToken>(
                    "/security/tokens",
                    {
                      name: name.trim(),
                      scope,
                      expires_in_days: Number(days) || 0,
                    }
                  );
                  setCreated(token);
                  setName("");
                  tokens.reload();
                })
              }
            >
              Create token
            </button>
          </div>
        </FormRow>
      </div>

      <div className="divider" />

      {/* ── Issued tokens ─────────────────────────────────────────────────── */}
      <div className="formsec">
        <div className="formsec__title">Issued tokens</div>

        {tokens.loading && !tokens.data ? (
          <Empty icon="shield" title="Loading" text="Reading issued tokens." />
        ) : rows.length === 0 ? (
          <Empty
            icon="shield"
            title="No tokens"
            text="Nothing but this app window can reach the API."
          />
        ) : (
          <table className="kvtable">
            <thead>
              <tr>
                <th>Name</th>
                <th>Scope</th>
                <th>Created</th>
                <th>Last used</th>
                <th>Expires</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {rows.map((t) => (
                <tr key={t.id} className={t.revoked_at ? "secrow--revoked" : ""}>
                  <td className="truncate" title={t.name}>
                    {t.name}
                  </td>
                  <td>
                    <span className={SCOPE_BADGES[t.scope] ?? "badge"}>
                      {t.scope}
                    </span>
                  </td>
                  <td title={dateTime(t.created_at)} style={{ whiteSpace: "nowrap" }}>
                    {relativeTime(t.created_at)}
                  </td>
                  <td
                    title={t.last_used_at ? dateTime(t.last_used_at) : undefined}
                    style={{ whiteSpace: "nowrap" }}
                  >
                    {t.last_used_at ? relativeTime(t.last_used_at) : "never"}
                  </td>
                  <td
                    title={t.expires_at ? dateTime(t.expires_at) : undefined}
                    style={{ whiteSpace: "nowrap" }}
                  >
                    {t.expires_at ? relativeTime(t.expires_at) : "never"}
                  </td>
                  <td style={{ textAlign: "right", whiteSpace: "nowrap" }}>
                    {t.revoked_at ? (
                      <span className="badge" title={dateTime(t.revoked_at)}>
                        revoked
                      </span>
                    ) : confirmRevoke === t.id ? (
                      <span className="row">
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
                      </span>
                    ) : (
                      <button
                        className="btn btn--ghost"
                        onClick={() => setConfirmRevoke(t.id)}
                      >
                        Revoke
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      <div className="divider" />

      <CredentialsCheckerSection checker={checker} />
    </div>
  );
}

/* ============================================================================
   Credentials Checker (#606) — the toggle and the whitelist.

   A whitelist entry names either a detection rule or one value. A value entry
   is stored as the SHA-256 of the matched text, and the server never sends a
   hash back, so the value typed here is hashed in this window and only the
   hash leaves it. The other way to whitelist one value — from its finding —
   is the Credentials Checker view's context menu (#605).
   ========================================================================== */

const WHITELIST_KINDS = [
  { value: "rule", label: "Detection rule" },
  { value: "value", label: "One value" },
];

/** SHA-256 of `text`, lowercase hex — `store::hash_match`'s scheme. */
async function sha256Hex(text: string): Promise<string> {
  if (!globalThis.crypto?.subtle) {
    throw new Error(
      "This window cannot hash a value. Whitelist it from its finding in the Credentials Checker instead."
    );
  }
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(text)
  );
  return Array.from(new Uint8Array(digest), (b) =>
    b.toString(16).padStart(2, "0")
  ).join("");
}

function CredentialsCheckerSection({ checker }: { checker: CheckerToggle }) {
  const whitelist = useResource<CredentialWhitelistEntry[] | null>(
    (signal) =>
      api.get<CredentialWhitelistEntry[] | null>(
        "/security-scan/whitelist",
        signal
      ),
    []
  );

  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [confirmDelete, setConfirmDelete] = useState<number>();
  const [kind, setKind] = useState("rule");
  const [target, setTarget] = useState("");
  const [reason, setReason] = useState("");

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

  const rows = whitelist.data ?? [];

  return (
    <div className="formsec">
      <div className="formsec__title">Credentials Checker</div>

      <div className="grouplist">
        <div className="grouplist__row">
          <div className="col" style={{ flex: 1, gap: 1 }}>
            <span style={{ fontWeight: 500 }}>Scan transcripts for credentials</span>
            <span style={{ fontSize: "var(--text-sm)", color: "var(--fg-tertiary)" }}>
              {checker.lock
                ? `Set by ${checker.lock}. Unset it to change this here.`
                : checker.error && checker.enabled === undefined
                  ? `Settings unavailable: ${checker.error}`
                  : "Looks for leaked secrets in Claude Code session transcripts, in the background. Findings are listed under Security → Credentials Checker."}
            </span>
          </div>
          <Switch
            on={checker.enabled ?? false}
            disabled={checker.enabled === undefined || !!checker.lock}
            onChange={checker.onChange}
          />
        </div>
      </div>

      {error && (
        <div className="msgline msgline--error">
          <Icon name="alert" size={14} className="msgline__icon" />
          <span>{error}</span>
        </div>
      )}

      <FormRow
        label="Whitelist"
        help="A rule entry silences every finding of that rule. A value entry silences one credential wherever it appears."
      >
        <Dropdown
          value={kind}
          options={WHITELIST_KINDS}
          onChange={(v) => {
            setKind(v);
            setTarget("");
          }}
        />
      </FormRow>

      {kind === "rule" ? (
        <FormRow label="Rule ID" help="As shown on a finding.">
          <input
            className="field mono"
            value={target}
            placeholder="e.g. aws-access-key-id"
            spellCheck={false}
            onChange={(e) => setTarget(e.target.value)}
          />
        </FormRow>
      ) : (
        <FormRow
          label="Value"
          help="The exact credential text. It is hashed in this window and only the hash is sent and stored — the value itself goes nowhere."
        >
          <input
            className="field mono"
            type="password"
            autoComplete="off"
            value={target}
            spellCheck={false}
            onChange={(e) => setTarget(e.target.value)}
          />
        </FormRow>
      )}

      <FormRow label="Reason" help="Optional. Shown in the list below.">
        <input
          className="field"
          value={reason}
          placeholder="e.g. test fixture key"
          onChange={(e) => setReason(e.target.value)}
        />
      </FormRow>

      <FormRow label="">
        <div className="row">
          <button
            className="btn btn--primary"
            disabled={busy || !target.trim()}
            onClick={() =>
              run(async () => {
                const body =
                  kind === "rule"
                    ? { rule_id: target.trim() }
                    : { match_hash: await sha256Hex(target.trim()) };
                await api.post<CredentialWhitelistEntry>(
                  "/security-scan/whitelist",
                  { ...body, reason: reason.trim() }
                );
                setTarget("");
                setReason("");
                whitelist.reload();
              })
            }
          >
            Create entry
          </button>
        </div>
      </FormRow>

      {whitelist.loading && !whitelist.data ? (
        <Empty icon="shield" title="Loading" text="Reading the whitelist." />
      ) : whitelist.error && !whitelist.data ? (
        <Empty
          icon="alert"
          title="Whitelist unavailable"
          text={whitelist.error}
          action={
            <button className="btn btn--lg" onClick={whitelist.reload}>
              <Icon name="refresh" size={14} />
              Retry
            </button>
          }
        />
      ) : rows.length === 0 ? (
        <Empty
          icon="shield"
          title="No whitelist entries"
          text="Every finding the checker makes is reported."
        />
      ) : (
        <table className="kvtable">
          <thead>
            <tr>
              <th>Kind</th>
              <th>Target</th>
              <th>Reason</th>
              <th>Created</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {rows.map((w) => (
              <tr key={w.id}>
                <td>
                  <span className={w.kind === "rule" ? "badge" : "badge badge--purple"}>
                    {w.kind}
                  </span>
                </td>
                <td className="truncate" title={w.rule_id ?? undefined}>
                  {w.rule_id ? (
                    <span className="mono">{w.rule_id}</span>
                  ) : (
                    <span style={{ color: "var(--fg-tertiary)" }}>
                      one value (hashed)
                    </span>
                  )}
                </td>
                <td className="truncate" title={w.reason ?? undefined}>
                  {w.reason ?? ""}
                </td>
                <td title={dateTime(w.created_at)} style={{ whiteSpace: "nowrap" }}>
                  {relativeTime(w.created_at)}
                </td>
                <td style={{ textAlign: "right", whiteSpace: "nowrap" }}>
                  {confirmDelete === w.id ? (
                    <span className="row">
                      <span className="confirm">
                        Delete this entry? Findings only it covers are reported
                        again.
                      </span>
                      <button
                        className="btn btn--ghost"
                        onClick={() => setConfirmDelete(undefined)}
                      >
                        Cancel
                      </button>
                      <button
                        className="btn btn--danger"
                        disabled={busy}
                        onClick={() =>
                          run(async () => {
                            await api.del(`/security-scan/whitelist/${w.id}`);
                            setConfirmDelete(undefined);
                            whitelist.reload();
                          })
                        }
                      >
                        Delete
                      </button>
                    </span>
                  ) : (
                    <button
                      className="btn btn--ghost"
                      onClick={() => setConfirmDelete(w.id)}
                    >
                      Delete
                    </button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
