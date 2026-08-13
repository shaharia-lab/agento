import { Icon } from "../../lib/icons";
import { duration, integer, percent, usd } from "../../lib/format";
import type { InsightsSummary } from "../../lib/types";
import { Card, RankList, Tile, share, widthPct } from "./shared";

/* ============================================================================
   Insights mode — how the sessions actually ran: autonomy, tooling, errors.
   Backed by /claude-sessions/insights/summary.
   ========================================================================== */

export function InsightsMode({ data }: { data: InsightsSummary }) {
  // Only a fraction of tool calls carry attribution, so the breakdown lists
  // describe a subset. Stating that up front stops the ranks reading as totals.
  const attributed = Math.max(0, data.total_tool_calls - data.unattributed_calls);
  const errorSessionShare = share(data.sessions_with_errors, data.total_sessions);

  return (
    <>
      <div className="tiles">
        <Tile
          icon="grid"
          label="Sessions analysed"
          value={integer(data.total_sessions)}
        />
        <Tile
          icon="sparkle"
          label="Avg autonomy"
          value={
            isFinite(data.avg_autonomy_score)
              ? data.avg_autonomy_score.toFixed(1)
              : "—"
          }
          note="0–100 score"
        />
        <Tile
          icon="chat"
          label="Avg turns"
          value={
            isFinite(data.avg_turn_count) ? data.avg_turn_count.toFixed(1) : "—"
          }
          note="user turns per session"
        />
        <Tile
          icon="terminal"
          label="Avg tool calls"
          value={
            isFinite(data.avg_tool_calls_total)
              ? data.avg_tool_calls_total.toFixed(0)
              : "—"
          }
          note={`${integer(data.total_tool_calls)} total`}
        />
        <Tile
          icon="layers"
          label="Cache hit rate"
          // This one arrives as a 0-1 fraction, unlike the analytics report's
          // cache_hit_rate which is already a percentage.
          value={percent(data.avg_cache_hit_rate * 100)}
          note="averaged per session"
        />
        <Tile
          icon="alert"
          label="Sessions with errors"
          value={integer(data.sessions_with_errors)}
          tone={errorSessionShare > 50 ? "var(--amber)" : undefined}
          note={`${percent(errorSessionShare, 0)} of sessions`}
        />
        <Tile
          icon="close"
          label="Tool errors"
          value={integer(data.total_tool_errors)}
          note="across all sessions"
        />
        <Tile
          icon="clock"
          label="Avg wall time"
          value={duration(data.avg_total_duration_ms)}
          note={`${duration(data.avg_active_duration_ms)} active`}
        />
        <Tile
          icon="dollar"
          label="Total cost"
          value={usd(data.total_cost_estimate_usd)}
          tone="var(--green)"
          note={`${usd(data.avg_cost_estimate_usd)} per session`}
        />
      </div>

      <Card
        title="Tool call attribution"
        sub="Which calls the breakdowns below can actually explain"
      >
        <div className="stackbar">
          <span
            className="stackbar__seg"
            style={{
              width: widthPct(attributed, data.total_tool_calls),
              background: "var(--accent)",
            }}
            title={`Attributed — ${integer(attributed)}`}
          />
          <span
            className="stackbar__seg"
            style={{
              width: widthPct(data.unattributed_calls, data.total_tool_calls),
              background: "var(--fg-quaternary)",
            }}
            title={`Unattributed — ${integer(data.unattributed_calls)}`}
          />
        </div>
        <div className="legend">
          <div className="legend__item">
            <div className="legend__key">
              <span
                className="legend__swatch"
                style={{ background: "var(--accent)" }}
              />
              Attributed
            </div>
            <div className="legend__val">{integer(attributed)}</div>
            <div className="legend__sub">
              {percent(share(attributed, data.total_tool_calls))} of all calls
            </div>
          </div>
          <div className="legend__item">
            <div className="legend__key">
              <span
                className="legend__swatch"
                style={{ background: "var(--fg-quaternary)" }}
              />
              Unattributed
            </div>
            <div className="legend__val">{integer(data.unattributed_calls)}</div>
            <div className="legend__sub">
              {percent(share(data.unattributed_calls, data.total_tool_calls))} of
              all calls
            </div>
          </div>
          <div className="legend__item">
            <div className="legend__key">
              <span
                className="legend__swatch"
                style={{ background: "transparent" }}
              />
              Total tool calls
            </div>
            <div className="legend__val">{integer(data.total_tool_calls)}</div>
            <div className="legend__sub">recorded across all sessions</div>
          </div>
        </div>
        {data.unattributed_calls > 0 && (
          <div
            className="a-note"
            style={{
              marginTop: "var(--sp-5)",
              display: "flex",
              gap: "var(--sp-3)",
              alignItems: "flex-start",
            }}
          >
            <Icon name="info" size={13} />
            <span>
              {percent(share(data.unattributed_calls, data.total_tool_calls))} of
              tool calls could not be tied to a skill, plugin, MCP server or
              subagent. The rankings below cover the remaining{" "}
              {integer(attributed)}.
            </span>
          </div>
        )}
      </Card>

      <div className="a-grid a-grid--2">
        <Card title="Top tools" sub="Direct tool invocations" table>
          <RankList entries={data.top_tools} emptyText="No tool calls recorded" />
        </Card>
        <Card title="Top agents" sub="Subagent types dispatched" table>
          <RankList entries={data.top_agents} emptyText="No subagents dispatched" />
        </Card>
      </div>

      <div className="a-grid a-grid--2">
        <Card title="Top skills" table>
          <RankList entries={data.top_skills} emptyText="No skills invoked" />
        </Card>
        <Card title="Top plugins" table>
          <RankList entries={data.top_plugins} emptyText="No plugins used" />
        </Card>
      </div>

      <div className="a-grid a-grid--2">
        <Card title="Top MCP servers" table>
          <RankList
            entries={data.top_mcp_servers}
            emptyText="No MCP servers used"
          />
        </Card>
        <Card title="Top MCP tools" table>
          <RankList entries={data.top_mcp_tools} emptyText="No MCP tools called" />
        </Card>
      </div>

      <Card title="Reasoning effort" sub="Calls by declared effort level" table>
        <RankList
          entries={data.top_efforts}
          emptyText="No effort levels recorded"
        />
      </Card>
    </>
  );
}

/** Inspector rows for this mode, so the shell can render them without the data
 *  leaking into the view's own layout. */
export function insightsInspectorRows(data: InsightsSummary) {
  return [
    { label: "Sessions", value: integer(data.total_sessions) },
    { label: "Avg autonomy", value: data.avg_autonomy_score.toFixed(1) },
    { label: "Avg turns", value: data.avg_turn_count.toFixed(1) },
    { label: "Cache hit", value: percent(data.avg_cache_hit_rate * 100) },
    {
      label: "Attributed",
      value: percent(
        share(
          Math.max(0, data.total_tool_calls - data.unattributed_calls),
          data.total_tool_calls
        )
      ),
    },
    { label: "Total cost", value: usd(data.total_cost_estimate_usd) },
  ];
}
