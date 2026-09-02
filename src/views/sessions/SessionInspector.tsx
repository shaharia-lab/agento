/**
 * The session inspector — the metadata groups of the right-hand pane (#538).
 *
 * It was `function Inspector` inside `views/SessionsView.tsx`, private to a
 * 2,000-line file even though it takes one prop and no handlers. It is a module
 * now for the same reason `components/SaveBar.tsx` is: a shape only one file can
 * reach is a shape the next caller copies instead of importing.
 *
 * **Actions are not here.** They live in `.sess-strip`, which `SessionsView`
 * renders *above* the scrolling pane this component fills, because as the last
 * group of a scrolling pane they were below the fold on any session with a full
 * metadata block — the whole of #486. Collapsing the data groups is the other
 * half of that same problem, and it does not move the strip.
 *
 * **The open/closed state is held here and written through to localStorage** on
 * every toggle (`lib/inspectorPrefs.ts`), so it survives a reload and a trip
 * out of the Sessions section — `App.tsx` renders each view conditionally, so
 * this component remounts on every arrival and re-seeds from storage.
 *
 * The `Session` group is deliberately not collapsible: it is the pane's
 * identity, not one of its figures.
 */
import { useCallback, useState } from "react";
import { CopyButton } from "../../components/CopyButton";
import { InspGroup, InspRow } from "../../components/ui";
import {
  compactNumber,
  dateTime,
  duration,
  integer,
  tildePath,
  usd,
} from "../../lib/format";
import {
  type InspectorGroupId,
  loadInspectorPrefs,
  saveInspectorPrefs,
} from "../../lib/inspectorPrefs";
import { sessionAgentName } from "../../lib/sessionAgent";
import { openExternal } from "../../lib/tauri";
import type { ClaudeSessionSummary } from "../../lib/types";
import {
  costOf,
  modeBadge,
  tokensIn,
  tokensOut,
  totalCost,
  totalDuration,
  usageOf,
} from "./sessionMetrics";

export function SessionInspector({
  session,
}: {
  session: ClaudeSessionSummary;
}) {
  const [open, setOpen] = useState(loadInspectorPrefs);

  const toggle = useCallback((id: InspectorGroupId) => {
    setOpen((prev) => {
      const next = { ...prev, [id]: !prev[id] };
      saveInspectorPrefs(next);
      return next;
    });
  }, []);

  const usage = usageOf(session.usage);
  const sub = usageOf(session.subagent_usage);
  const cost = costOf(session.cost);
  const subCost = costOf(session.subagent_cost);
  const prs = session.prs ?? [];
  const badge = modeBadge(session);
  const agentName = sessionAgentName(session);

  return (
    <>
      <InspGroup title="Session">
        <div className="sess-heading">
          {session.display_title || "Untitled session"}
        </div>
        {session.preview && (
          <div className="sess-preview">{session.preview}</div>
        )}
        {/* Both of these are single values a user copies whole and neither
            fits the pane, so the button carries the real string while the row
            shows an abbreviated one (#469). */}
        <InspRow label="ID">
          <span className="row insp-row__copy">
            <span className="mono truncate">{session.session_id}</span>
            <CopyButton text={session.session_id} title="Copy session ID" />
          </span>
        </InspRow>
        {session.custom_title && (
          <InspRow label="Custom">{session.custom_title}</InspRow>
        )}
        {session.ai_title && session.ai_title !== session.display_title && (
          <InspRow label="AI title">{session.ai_title}</InspRow>
        )}
        {session.native_title && (
          <InspRow label="Native">{session.native_title}</InspRow>
        )}
        {/* Another name Claude Code recorded for the session, shown only when
            it is not one of the titles above — the same suppression the AI
            title gets, for the reasons in lib/sessionAgent.ts. It was labelled
            "Agent" and sat beside Model and Config until it turned out never to
            name an agent, so it belongs here with the other titles. */}
        {agentName && <InspRow label="Named">{agentName}</InspRow>}
        <InspRow label="Project">
          <span className="row insp-row__copy">
            <span className="truncate" title={session.project_path}>
              {tildePath(session.project_path)}
            </span>
            <CopyButton
              text={session.project_path}
              title="Copy the project path"
            />
          </span>
        </InspRow>
        {session.cwd && session.cwd !== session.project_path && (
          <InspRow label="Directory">{tildePath(session.cwd)}</InspRow>
        )}
        {session.relocated_cwd && (
          <InspRow label="Relocated">{tildePath(session.relocated_cwd)}</InspRow>
        )}
        <InspRow label="Branch">{session.git_branch || "—"}</InspRow>
        {session.worktree_name && (
          <InspRow label="Worktree">
            {session.worktree_name}
            {session.worktree_branch ? ` · ${session.worktree_branch}` : ""}
          </InspRow>
        )}
        {session.original_branch && (
          <InspRow label="From">{session.original_branch}</InspRow>
        )}
        <InspRow label="Model">{session.model || "—"}</InspRow>
        <InspRow label="Mode">
          {badge ? (
            <span className={`badge ${badge.tone}`}>{badge.label}</span>
          ) : (
            "—"
          )}
        </InspRow>
        <InspRow label="Config">{session.config_dir ?? "Default"}</InspRow>
      </InspGroup>

      <InspGroup
        title="Activity"
        collapsible
        open={open.activity}
        onToggle={() => toggle("activity")}
      >
        <InspRow label="Started">{dateTime(session.start_time)}</InspRow>
        <InspRow label="Last">{dateTime(session.last_activity)}</InspRow>
        <InspRow label="Active">
          <span className="tnum">{duration(session.active_duration_ms)}</span>
        </InspRow>
        <InspRow label="Sub-agents">
          <span className="tnum">
            {duration(session.subagent_active_duration_ms)}
          </span>
        </InspRow>
        <InspRow label="Total">
          <span className="tnum">{duration(totalDuration(session))}</span>
        </InspRow>
        <InspRow label="Messages">
          <span className="tnum">{integer(session.message_count)}</span>
        </InspRow>
        <InspRow label="Events">
          <span className="tnum">{integer(session.event_count)}</span>
        </InspRow>
        <InspRow label="Compactions">
          <span className="tnum">{integer(session.compaction_count)}</span>
        </InspRow>
        {session.dropped_tokens > 0 && (
          <InspRow label="Dropped">
            <span className="tnum">{integer(session.dropped_tokens)}</span>
          </InspRow>
        )}
      </InspGroup>

      <InspGroup
        title="Tokens"
        collapsible
        open={open.tokens}
        onToggle={() => toggle("tokens")}
      >
        <InspRow label="Input">
          <span className="tnum">{integer(usage.input_tokens)}</span>
        </InspRow>
        <InspRow label="Output">
          <span className="tnum">{integer(usage.output_tokens)}</span>
        </InspRow>
        <InspRow label="Cache read">
          <span className="tnum">{integer(usage.cache_read_tokens)}</span>
        </InspRow>
        <InspRow label="Cache write">
          <span className="tnum">{integer(usage.cache_creation_tokens)}</span>
        </InspRow>
        <InspRow label="Billable">
          <span className="tnum">
            {integer(tokensIn(session))} in / {integer(tokensOut(session))} out
          </span>
        </InspRow>
      </InspGroup>

      {/* The count stays in the title of both of these, so a collapsed group
          still carries every figure its header carried open. */}
      <InspGroup
        title={`Sub-agents · ${integer(session.subagent_count)}`}
        collapsible
        open={open.subagents}
        onToggle={() => toggle("subagents")}
      >
        <InspRow label="Input">
          <span className="tnum">{integer(sub.input_tokens)}</span>
        </InspRow>
        <InspRow label="Output">
          <span className="tnum">{integer(sub.output_tokens)}</span>
        </InspRow>
        <InspRow label="Cache read">
          <span className="tnum">{integer(sub.cache_read_tokens)}</span>
        </InspRow>
        <InspRow label="Cache write">
          <span className="tnum">{integer(sub.cache_creation_tokens)}</span>
        </InspRow>
        <InspRow label="Cost">
          <span className="tnum">{usd(subCost.total_usd)}</span>
        </InspRow>
      </InspGroup>

      <InspGroup
        title="Cost"
        collapsible
        open={open.cost}
        onToggle={() => toggle("cost")}
      >
        <InspRow label="Input">
          <span className="tnum">{usd(cost.input_usd)}</span>
        </InspRow>
        <InspRow label="Output">
          <span className="tnum">{usd(cost.output_usd)}</span>
        </InspRow>
        <InspRow label="Cache read">
          <span className="tnum">{usd(cost.cache_read_usd)}</span>
        </InspRow>
        <InspRow label="Cache write">
          <span className="tnum">{usd(cost.cache_write_usd)}</span>
        </InspRow>
        <InspRow label="Session">
          <span className="tnum">{usd(cost.total_usd)}</span>
        </InspRow>
        <InspRow label="Sub-agents">
          <span className="tnum">{usd(subCost.total_usd)}</span>
        </InspRow>
        <InspRow label="Total">
          <span className="tnum" style={{ color: "var(--green)" }}>
            {usd(totalCost(session))}
          </span>
        </InspRow>
        {session.unpriced_models?.length ? (
          <InspRow label="Unpriced">
            <span title={session.unpriced_models.join(", ")}>
              {session.unpriced_models.length} models ·{" "}
              {compactNumber(session.unpriced_tokens ?? 0)} tokens
            </span>
          </InspRow>
        ) : null}
      </InspGroup>

      {prs.length > 0 && (
        <InspGroup
          title={`Pull requests · ${prs.length}`}
          collapsible
          open={open.prs}
          onToggle={() => toggle("prs")}
        >
          <div className="sess-prs">
            {prs.map((pr) => (
              <div className="sess-pr" key={`${pr.pr_repository}#${pr.pr_number}`}>
                <a
                  href={pr.pr_url}
                  onClick={(e) => {
                    e.preventDefault();
                    openExternal(pr.pr_url);
                  }}
                >
                  #{pr.pr_number}
                </a>
                <span className="sess-pr__repo">{pr.pr_repository}</span>
              </div>
            ))}
          </div>
        </InspGroup>
      )}
    </>
  );
}
