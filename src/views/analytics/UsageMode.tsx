import { compactNumber, integer, percent, usd, duration } from "../../lib/format";
import type { AnalyticsReport, SessionRanking } from "../../lib/types";
import { AreaChart, BarChart, Heatmap, type Point } from "../../components/charts";
import { SessionLink } from "../sessions/SessionLink";
import {
  Card,
  CardEmpty,
  Delta,
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

export function UsageMode({
  report,
  previous,
}: {
  report: AnalyticsReport;
  /** The preceding window, when the comparison toggle is on and it loaded. */
  previous?: AnalyticsReport;
}) {
  const s = report.summary;
  const g = report.granularity;
  const was = previous?.summary;

  const series = trimEmpty(list(report.time_series), (p) => p.sessions);
  const activeBuckets = series.filter((p) => p.sessions > 0).length;

  const sessionPoints: Point[] = series.map((p) => ({
    label: bucketLabel(p.date, g),
    value: p.sessions,
    hint: `${bucketLabel(p.date, g)} — ${p.sessions} session${
      p.sessions === 1 ? "" : "s"
    } · ${compactNumber(p.total_tokens)} tokens`,
  }));

  // Trimmed on the same rule as the current series, so the two are like for
  // like; `AreaChart` then aligns them from the last bucket backwards.
  const compareSeries = previous
    ? trimEmpty(list(previous.time_series), (p) => p.sessions).map((p) => ({
        date: p.date,
        sessions: p.sessions,
      }))
    : undefined;
  const compareGranularity = previous?.granularity ?? g;
  const comparePoints: Point[] | undefined = compareSeries?.map((p) => ({
    label: bucketLabel(p.date, compareGranularity),
    value: p.sessions,
  }));

  // Averaging over buckets is only meaningful when the buckets are days.
  const avgPerDay =
    g === "daily" && activeBuckets > 0 ? s.total_sessions / activeBuckets : 0;

  // The same figure over the previous window, computed the same way — and only
  // when *both* windows bucket daily, since an average over weekly buckets is
  // not the same quantity and differencing the two would be nonsense.
  const compareActiveBuckets =
    compareSeries?.filter((p) => p.sessions > 0).length ?? 0;
  const comparableAvgPerDay =
    was !== undefined &&
    g === "daily" &&
    compareGranularity === "daily" &&
    compareActiveBuckets > 0
      ? was.total_sessions / compareActiveBuckets
      : undefined;

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
          delta={
            was === undefined ? undefined : (
              <Delta
                current={s.total_sessions}
                previous={was.total_sessions}
                format={integer}
              />
            )
          }
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
          delta={
            comparableAvgPerDay === undefined ? undefined : (
              <Delta
                current={avgPerDay}
                previous={comparableAvgPerDay}
                format={(n) => n.toFixed(1)}
              />
            )
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
          delta={
            was === undefined ? undefined : (
              <Delta
                current={s.unique_projects}
                previous={was.unique_projects}
                format={integer}
              />
            )
          }
        />
      </div>

      <div className="a-grid a-grid--wide">
        <Card title="Sessions over time" badge={granularityLabel(g)}>
          <AreaChart
            points={sessionPoints}
            compare={comparePoints}
            format={integer}
          />
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
                <td className="truncate" style={{ maxWidth: 320 }}>
                  {/* No `projectPath`: `SessionRanking.project` is analytics'
                      `decoded_path`, which is not the sessions list's
                      `project_path` — so "Copy project path" waits for the
                      hydrated row rather than copying the wrong string. */}
                  <SessionLink sessionId={r.session_id} title={r.title} />
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
