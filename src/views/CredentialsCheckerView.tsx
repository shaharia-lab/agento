/**
 * The Credentials Checker (#605, epic #597): what the background scan found
 * in Claude session transcripts, grouped by session, and the three verdicts a
 * user can give a finding.
 *
 * Everything here is a read of `GET /api/security-scan/findings` plus three
 * writes (#604). The verdicts are instant — no confirmation — because a
 * whitelist entry is reversible from Settings, and "false positive" is the
 * user's own call about their own data.
 *
 * The session is a `SessionLink` on the group's header rather than a column:
 * a left-click hands off to Sessions, and its right-click opens the session's
 * own menu there — that menu is not duplicated into the finding's.
 */
import { Fragment, useCallback, useMemo, useState } from "react";
import { api } from "../lib/api";
import type { CredentialFinding, CredentialFindingStatus } from "../lib/types";
import { describeError, useResource } from "../lib/hooks";
import { dateTime, relativeTime, tildePath } from "../lib/format";
import { Icon } from "../lib/icons";
import { ContextMenu, Empty, type ContextMenuItem } from "../components/ui";
import { SessionLink } from "./sessions/SessionLink";
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
 */
function groupBySession(rows: CredentialFinding[]): [string, CredentialFinding[]][] {
  const groups = new Map<string, CredentialFinding[]>();
  for (const f of rows) {
    const group = groups.get(f.session_id);
    if (group) group.push(f);
    else groups.set(f.session_id, [f]);
  }
  return [...groups];
}

export function CredentialsCheckerView() {
  const findings = useResource<CredentialFinding[] | null>(
    (signal) => api.get("/security-scan/findings", signal),
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
      } catch (err) {
        setActionError(describeError(err));
      } finally {
        setBusy(false);
      }
    },
    [findings]
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
          icon: "shield",
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

  return (
    <div className="panes">
      <div className="pane-detail">
        <div className="toolbar">
          <span className="toolbar__sub tnum">
            {rows.length} {rows.length === 1 ? "finding" : "findings"}
            {rows.length > 0 ? ` · ${openCount} open` : ""}
          </span>
          <div className="spacer" />
          {actionError && <span className="formerror">{actionError}</span>}
          <button className="iconbtn" title="Refresh" onClick={findings.reload}>
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
              <button className="btn" onClick={findings.reload}>
                <Icon name="refresh" size={13} />
                Retry
              </button>
            }
          />
        ) : rows.length === 0 ? (
          <Empty
            icon="key"
            title="No findings"
            text="Credentials the background scan finds in Claude session transcripts land here, grouped by session."
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
                {groups.map(([sessionId, items]) => (
                  <Fragment key={sessionId}>
                    <tr className="rowgroup cc-group">
                      <td colSpan={5}>
                        <div className="cc-group__head">
                          <span className="cc-group__session">
                            <SessionLink
                              sessionId={sessionId}
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
