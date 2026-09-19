/**
 * The Credentials Checker (#605, epic #597): what the background scan found
 * in Claude session transcripts, grouped by session, and the three verdicts a
 * user can give a finding.
 *
 * Two reads — `GET /api/security-scan/findings` and `…/status` — and three
 * writes (#604). The verdicts are instant, with no confirmation, like every
 * other row action in a context menu here. Nothing in this view undoes one:
 * a false positive is permanent through the API, and a whitelist entry is
 * removed with `DELETE /api/security-scan/whitelist/{id}`, whose UI is the
 * Settings → Security whitelist (#606).
 *
 * The status read is what keeps the empty state honest: the checker is off by
 * default, and "no findings" from a scan that never ran must not read as a
 * clean result.
 *
 * The session is a `SessionLink` on the group's header rather than a column:
 * a left-click hands off to Sessions, and its right-click opens the session's
 * own menu there — that menu is not duplicated into the finding's.
 */
import { Fragment, useCallback, useMemo, useState } from "react";
import { api } from "../lib/api";
import type {
  CredentialFinding,
  CredentialFindingStatus,
  CredentialsCheckerStatus,
} from "../lib/types";
import { describeError, useResource } from "../lib/hooks";
import { dateTime, relativeTime, tildePath } from "../lib/format";
import { Icon } from "../lib/icons";
import { ContextMenu, Empty, type ContextMenuItem } from "../components/ui";
import { SessionLink } from "./sessions/SessionLink";
// `.statepane` and `.formerror` are declared there, as for `JobsView`.
import "../styles/tasks.css";
import "../styles/credentialschecker.css";

const STATUS_LABEL: Record<CredentialFindingStatus, string> = {
  open: "Open",
  whitelisted: "Whitelisted",
  false_positive: "False positive",
};

function StatusBadge({ status }: { status: CredentialFindingStatus }) {
  const tone = status === "open" ? " badge--red" : "";
  return <span className={`badge${tone}`}>{STATUS_LABEL[status] ?? status}</span>;
}

function Confidence({ level }: { level: CredentialFinding["confidence"] }) {
  return (
    <span className="cc-confidence">
      <span className={`dot ${level === "high" ? "dot--red" : "dot--amber"}`} />
      {level === "high" ? "High" : "Medium"}
    </span>
  );
}

/**
 * Findings by session, in the order the server sent them (newest first), so a
 * group sits where its most recent finding would.
 *
 * Keyed on the id **and** the project path, as the store is: one session id can
 * appear under two project paths, and those are two groups, not one.
 */
function groupBySession(rows: CredentialFinding[]): [string, CredentialFinding[]][] {
  const groups = new Map<string, CredentialFinding[]>();
  for (const f of rows) {
    const key = `${f.session_id}\0${f.project_path}`;
    const group = groups.get(key);
    if (group) group.push(f);
    else groups.set(key, [f]);
  }
  return [...groups];
}

export function CredentialsCheckerView() {
  const findings = useResource<CredentialFinding[] | null>(
    (signal) => api.get("/security-scan/findings", signal),
    []
  );
  const status = useResource<CredentialsCheckerStatus>(
    (signal) => api.get("/security-scan/status", signal),
    []
  );
  const [selected, setSelected] = useState<number>();
  const [menu, setMenu] = useState<{ at: { x: number; y: number }; id: number }>();
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<string>();

  const rows = useMemo(() => findings.data ?? [], [findings.data]);
  const groups = useMemo(() => groupBySession(rows), [rows]);
  const openCount = rows.filter((f) => f.status === "open").length;

  /** One verdict, then a fresh read: a whitelist entry can change other rows. */
  const act = useCallback(
    async (path: string, body?: unknown) => {
      setBusy(true);
      setActionError(undefined);
      try {
        await api.post<unknown>(path, body);
        findings.reload();
        status.reload();
      } catch (err) {
        setActionError(describeError(err));
      } finally {
        setBusy(false);
      }
    },
    [findings, status]
  );

  const menuFinding = menu && rows.find((f) => f.id === menu.id);
  const menuItems: ContextMenuItem[] = menuFinding
    ? [
        {
          label: "Mark as false positive",
          icon: "check",
          // Permanent through this surface: nothing moves a row back out.
          disabled: busy || menuFinding.status === "false_positive",
          onSelect: () =>
            void act(`/security-scan/findings/${menuFinding.id}/false-positive`),
        },
        {
          label: "Whitelist this value",
          icon: "key",
          disabled: busy,
          onSelect: () =>
            void act(`/security-scan/findings/${menuFinding.id}/whitelist`),
        },
        {
          label: "Whitelist this rule",
          icon: "filter",
          disabled: busy,
          onSelect: () =>
            void act("/security-scan/whitelist", { rule_id: menuFinding.rule_id }),
        },
      ]
    : [];

  const loading = findings.loading && !findings.data;
  const off = status.data ? !status.data.enabled : undefined;

  const refresh = () => {
    findings.reload();
    status.reload();
  };

  return (
    <div className="panes">
      <div className="pane-detail">
        <div className="toolbar">
          <span className="toolbar__sub tnum">
            {rows.length} {rows.length === 1 ? "finding" : "findings"}
            {rows.length > 0 ? ` · ${openCount} open` : ""}
            {off ? " · checker off" : ""}
            {off === false && !status.data?.running ? " · scan not running" : ""}
            {status.data?.last_scanned_at
              ? ` · last scan ${relativeTime(status.data.last_scanned_at)}`
              : ""}
          </span>
          <div className="spacer" />
          {actionError && <span className="formerror">{actionError}</span>}
          {status.error && (
            <span className="formerror">
              Couldn't read the checker's status: {status.error}
            </span>
          )}
          <button className="iconbtn" title="Refresh" onClick={refresh}>
            <Icon name="refresh" size={14} />
          </button>
        </div>

        {loading ? (
          <div className="statepane">Loading findings…</div>
        ) : findings.error ? (
          <Empty
            icon="alert"
            title="Couldn't load findings"
            text={findings.error}
            action={
              <button className="btn" onClick={refresh}>
                <Icon name="refresh" size={13} />
                Retry
              </button>
            }
          />
        ) : rows.length === 0 && off ? (
          <Empty
            icon="key"
            title="The Credentials Checker is off"
            text="The background scan is switched off, so no session has been checked for leaked credentials."
          />
        ) : rows.length === 0 && off === undefined ? (
          // The status read failed or is still in flight: whether anything was
          // scanned is unknown, and an empty list must not claim a clean result.
          <div className="statepane">
            {status.error ? "Couldn't tell whether the checker has run." : "Loading…"}
          </div>
        ) : rows.length === 0 && !status.data?.last_scanned_at ? (
          <Empty
            icon="key"
            title="Not scanned yet"
            text={
              status.data?.running
                ? "The first scan is running. Credentials it finds in Claude session transcripts land here, grouped by session."
                : "The checker is on, but its background scan isn't running, so no session has been checked yet."
            }
          />
        ) : rows.length === 0 ? (
          <Empty
            icon="key"
            title="No findings"
            text={
              status.data?.running
                ? "Credentials the background scan finds in Claude session transcripts land here, grouped by session."
                : `Nothing was found as of the last scan, ${relativeTime(status.data?.last_scanned_at)}. The background scan isn't running now, so newer sessions haven't been checked.`
            }
          />
        ) : (
          <div className="scroll" style={{ flex: 1, minHeight: 0 }}>
            <table className="table table--striped cc-table">
              <thead>
                <tr>
                  <th style={{ width: "22%" }}>Rule</th>
                  <th style={{ width: 104 }}>Confidence</th>
                  <th>Snippet</th>
                  <th style={{ width: 108 }}>Detected</th>
                  <th style={{ width: 140 }}>Status</th>
                </tr>
              </thead>
              <tbody>
                {groups.map(([key, items]) => (
                  <Fragment key={key}>
                    <tr className="rowgroup cc-group">
                      <td colSpan={5}>
                        <div className="cc-group__head">
                          <span className="cc-group__session">
                            <SessionLink
                              sessionId={items[0].session_id}
                              projectPath={items[0].project_path || undefined}
                            />
                          </span>
                          <span className="cc-group__meta truncate">
                            {tildePath(items[0].project_path)} · {items.length}{" "}
                            {items.length === 1 ? "finding" : "findings"}
                          </span>
                        </div>
                      </td>
                    </tr>
                    {items.map((f) => (
                      <tr
                        key={f.id}
                        className={selected === f.id ? "is-selected" : ""}
                        onClick={() => setSelected(f.id)}
                        onContextMenu={(e) => {
                          e.preventDefault();
                          setSelected(f.id);
                          setMenu({ at: { x: e.clientX, y: e.clientY }, id: f.id });
                        }}
                      >
                        <td className="truncate" title={f.rule_id}>
                          {f.rule_id}
                        </td>
                        <td>
                          <Confidence level={f.confidence} />
                        </td>
                        <td className="truncate" title={f.masked_snippet}>
                          <span className="mono">{f.masked_snippet}</span>
                        </td>
                        <td className="tnum" title={dateTime(f.detected_at)}>
                          {relativeTime(f.detected_at)}
                        </td>
                        <td>
                          <StatusBadge status={f.status} />
                        </td>
                      </tr>
                    ))}
                  </Fragment>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>

      {menu && menuFinding && (
        <ContextMenu at={menu.at} items={menuItems} onClose={() => setMenu(undefined)} />
      )}
    </div>
  );
}
