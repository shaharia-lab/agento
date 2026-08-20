import { compactNumber, integer, percent, usd, duration } from "../../lib/format";
import type { AnalyticsReport, SessionRanking } from "../../lib/types";
import { AreaChart, BarChart, Heatmap, type Point } from "./charts";
import {
  Card,
  CardEmpty,
  Tile,
  Tokens,
  bucketLabel,
  granularityLabel,
  list,
  projectLabel,
  share,
  trimEmpty,
  widthPct,
} from "./shared";

/* ============================================================================
   Usage mode — when the work happened, not what it cost.
   ========================================================================== */

export function UsageMode({ report }: { report: AnalyticsReport }) {
  const s = report.summary;
  const g = report.granularity;

  const series = trimEmpty(list(report.time_series), (p) => p.sessions);
  const activeBuckets = series.filter((p) => p.sessions > 0).length;

  const sessionPoints: Point[] = series.map((p) => ({
    label: bucketLabel(p.date, g),
    value: p.sessions,
    hint: `${bucketLabel(p.date, g)} — ${p.sessions} session${
      p.sessions === 1 ? "" : "s"
    } · ${compactNumber(p.total_tokens)} tokens`,
  }));

  // Averaging over buckets is only meaningful when the buckets are days.
  const avgPerDay =
    g === "daily" && activeBuckets > 0 ? s.total_sessions / activeBuckets : 0;

  const perModel = list(report.sessions_per_model);
  const topModel = perModel.reduce<{ model: string; sessions: number } | undefined>(
    (best, m) => (!best || m.sessions > best.sessions ? m : best),
    undefined
  );
  const modelTotal = perModel.reduce((sum, m) => sum + m.sessions, 0);

  const hourly = list(report.hourly_activity);
  const hourPoints: Point[] = hourly.map((h) => ({
    label: `${String(h.hour).padStart(2, "0")}:00`,
    value: h.sessions,
    hint: `${String(h.hour).padStart(2, "0")}:00 — ${h.sessions} session${
      h.sessions === 1 ? "" : "s"
    } · ${compactNumber(h.tokens)} tokens`,
  }));

  // The server orders these by tokens; the card is about activity, so it is
  // re-sorted by session count to match its own label.
  const activeDays = [...list(report.most_active_days)]
    .sort((a, b) => b.sessions - a.sessions)
    .slice(0, 12);
  const dayPoints: Point[] = activeDays.map((d) => ({
    label: bucketLabel(d.date, "daily"),
    value: d.sessions,
    hint: `${d.date} — ${d.sessions} session${
      d.sessions === 1 ? "" : "s"
    } · ${compactNumber(d.tokens)} tokens`,
  }));

  return (
    <>
      <div className="tiles">
        <Tile
          icon="grid"
          label="Total sessions"
          value={integer(s.total_sessions)}
        />
        <Tile
          icon="clock"
          label="Avg sessions / day"
          value={avgPerDay > 0 ? avgPerDay.toFixed(1) : "—"}
          note={
            g === "daily"
              ? `over ${activeBuckets} active day${activeBuckets === 1 ? "" : "s"}`
              : `buckets are ${g}`
          }
        />
        <Tile
          icon="cpu"
          label="Most used model"
          value={s.most_used_model || "—"}
          note={
            topModel && modelTotal > 0
              ? `${percent(share(topModel.sessions, modelTotal))} of runs`
              : undefined
          }
          title={s.most_used_model}
        />
        <Tile
          icon="folder"
          label="Unique projects"
          value={integer(s.unique_projects)}
        />
      </div>

      <div className="a-grid a-grid--wide">
        <Card title="Sessions over time" badge={granularityLabel(g)}>
          <AreaChart points={sessionPoints} format={integer} />
        </Card>
        <Card title="Sessions by model" table>
          {perModel.length ? (
            <div className="a-rank">
              {perModel.map((m) => (
                <div className="a-rank__row" key={m.model}>
                  <div className="a-rank__name">
                    <div className="truncate" title={m.model}>
                      {m.model}
                    </div>
                    <div className="a-rank__bar">
                      <div
                        className="a-rank__fill"
                        style={{ width: widthPct(m.sessions, modelTotal) }}
                      />
                    </div>
                  </div>
                  <div className="a-rank__count">
                    {integer(m.sessions)}
                    <span style={{ color: "var(--fg-quaternary)" }}>
                      {" "}
                      {percent(share(m.sessions, modelTotal), 0)}
                    </span>
                  </div>
                </div>
              ))}
            </div>
          ) : (
            <CardEmpty text="No model activity in this period" />
          )}
        </Card>
      </div>

      <div className="a-grid a-grid--2">
        <Card title="Busiest days" sub="Ranked by session count">
          <BarChart points={dayPoints} format={integer} />
        </Card>
        <Card title="Hour of day" sub="Sessions active in each hour">
          <BarChart points={hourPoints} format={integer} tone="var(--teal)" />
        </Card>
      </div>

      <Card
        title="Weekly rhythm"
        sub="Sessions active in each day-of-week / hour slot"
      >
        <Heatmap cells={list(report.heatmap)} />
      </Card>

      <TopSessions
        title="Top sessions by cost"
        rows={report.top_sessions?.by_cost}
        rankedBy="cost"
      />
      <TopSessions
        title="Top sessions by duration"
        rows={report.top_sessions?.by_duration}
        rankedBy="duration"
      />
      <TopSessions
        title="Top sessions by tokens"
        rows={report.top_sessions?.by_tokens}
        rankedBy="tokens"
      />
    </>
  );
}

type RankedBy = "cost" | "duration" | "tokens";

/**
 * Every row carries all three metrics, so the ranking column is emphasised
 * rather than repeated — the rows already arrive in the server's rank order.
 */
function TopSessions({
  title,
  rows,
  rankedBy,
}: {
  title: string;
  rows: SessionRanking[] | null | undefined;
  rankedBy: RankedBy;
}) {
  const items = list(rows);
  const rankStyle = (col: RankedBy) =>
    col === rankedBy
      ? { color: "var(--fg)", fontWeight: "var(--weight-semibold)" }
      : { color: "var(--fg-secondary)" };

  return (
    <Card title={title} sub={`Ranked by ${rankedBy}`} table>
      {items.length ? (
        <table className="table table--striped">
          <thead>
            <tr>
              <th style={{ width: 40 }} className="num">
                #
              </th>
              <th>Session</th>
              <th>Project</th>
              <th>Model</th>
              <th className="num">Cost</th>
              <th className="num">Duration</th>
              <th className="num">Tokens</th>
              <th className="num">Subagents</th>
            </tr>
          </thead>
          <tbody>
            {items.map((r, i) => (
              <tr key={r.session_id}>
                <td className="num" style={{ color: "var(--fg-quaternary)" }}>
                  {i + 1}
                </td>
                <td
                  className="truncate"
                  style={{ maxWidth: 320 }}
                  title={r.title || r.session_id}
                >
                  {r.title || r.session_id}
                </td>
                <td
                  className="truncate"
                  style={{ maxWidth: 200, color: "var(--fg-tertiary)" }}
                  title={r.project}
                >
                  {projectLabel(r.project)}
                </td>
                <td className="truncate" style={{ color: "var(--fg-tertiary)" }}>
                  {r.model || "—"}
                </td>
                <td className="num" style={rankStyle("cost")}>
                  {usd(r.cost_usd)}
                </td>
                <td className="num" style={rankStyle("duration")}>
                  {duration(r.duration_ms)}
                </td>
                <td className="num" style={rankStyle("tokens")}>
                  <Tokens n={r.tokens} />
                </td>
                <td className="num">{integer(r.subagent_count)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : (
        <CardEmpty text="No sessions in this period" />
      )}
    </Card>
  );
}
