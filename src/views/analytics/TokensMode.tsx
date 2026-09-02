import { Icon } from "../../lib/icons";
import { compactNumber, duration, integer, percent, usd } from "../../lib/format";
import type { AnalyticsReport, InsightCard } from "../../lib/types";
import { AreaChart, RateChart, type Point } from "../../components/charts";
import {
  Card,
  CardEmpty,
  Delta,
  DeltaNote,
  Tile,
  Tokens,
  alignedPair,
  bucketLabel,
  granularityLabel,
  list,
  projectLabel,
  share,
  trimEmpty,
  widthPct,
} from "./shared";

/* ============================================================================
   Tokens mode — where the tokens went and what they cost.
   ========================================================================== */

/** The four token classes the estimated cost is computed over. */
const COMPOSITION = [
  { key: "Input", color: "var(--accent)", field: "total_input_tokens" },
  { key: "Output", color: "var(--purple)", field: "total_output_tokens" },
  { key: "Cache read", color: "var(--teal)", field: "total_cache_read_tokens" },
  { key: "Cache write", color: "var(--amber)", field: "total_cache_creation_tokens" },
] as const;

export function TokensMode({
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

  /** A tile's delta slot, or nothing at all when there is no comparison. */
  const vs = (current: number, before: number | undefined, format = compactNumber) =>
    before === undefined ? undefined : (
      <Delta current={current} previous={before} format={format} />
    );

  // `total_tokens` is conversation-only (input + output); the cost is computed
  // over all four classes, so the composition bar needs its own denominator.
  const billable =
    s.total_input_tokens +
    s.total_output_tokens +
    s.total_cache_read_tokens +
    s.total_cache_creation_tokens;

  // One call, so the two series cannot be windowed on different predicates.
  const { series, compare: compareSeries } = alignedPair(
    list(report.time_series),
    previous ? list(previous.time_series) : undefined,
    (p) => p.total_tokens
  );
  const tokenPoints: Point[] = series.map((p) => ({
    label: bucketLabel(p.date, g),
    value: p.total_tokens,
    hint: `${bucketLabel(p.date, g)} — ${integer(p.total_tokens)} tokens · ${
      p.sessions
    } session${p.sessions === 1 ? "" : "s"}`,
  }));

  const comparePoints: Point[] | undefined =
    previous && compareSeries
      ? compareSeries.map((p) => ({
          label: bucketLabel(p.date, previous.granularity),
          value: p.total_tokens,
        }))
      : undefined;

  const cache = trimEmpty(
    list(report.cache_efficiency),
    (p) => p.total_input_tokens
  );
  const cachePoints: Point[] = cache.map((p) => ({
    label: bucketLabel(p.date, g),
    value: p.cache_hit_rate,
    hint: `${bucketLabel(p.date, g)} — ${percent(p.cache_hit_rate)} of ${compactNumber(
      p.total_input_tokens
    )} input tokens served from cache`,
  }));

  const costs = list(report.cost_by_model);
  const projects = list(report.project_breakdown);
  const unpriced = s.unknown_pricing_tokens > 0;

  // A cost delta is only honest when *both* windows are fully priced: a model
  // that lost its rate between them makes an unchanged spend read as a fall, and
  // one that gained a rate makes it read as a rise. Neither is what happened, so
  // the figure is withheld and the reason is put in its place.
  const costComparable =
    was !== undefined && !unpriced && was.unknown_pricing_tokens === 0;

  return (
    <>
      <div className="tiles">
        <Tile
          icon="grid"
          label="Total sessions"
          value={integer(s.total_sessions)}
          note={`${integer(s.unique_projects)} projects`}
          delta={vs(s.total_sessions, was?.total_sessions, integer)}
        />
        <Tile
          icon="zap"
          label="Conversation tokens"
          value={compactNumber(s.total_tokens)}
          note="input + output"
          delta={vs(s.total_tokens, was?.total_tokens)}
          title={`${integer(s.total_tokens)} tokens`}
        />
        <Tile
          icon="arrowDown"
          label="Input"
          value={compactNumber(s.total_input_tokens)}
          note={percent(share(s.total_input_tokens, billable))}
          delta={vs(s.total_input_tokens, was?.total_input_tokens)}
          title={`${integer(s.total_input_tokens)} tokens`}
        />
        <Tile
          icon="arrowUp"
          label="Output"
          value={compactNumber(s.total_output_tokens)}
          note={percent(share(s.total_output_tokens, billable))}
          delta={vs(s.total_output_tokens, was?.total_output_tokens)}
          title={`${integer(s.total_output_tokens)} tokens`}
        />
        <Tile
          icon="layers"
          label="Cache read"
          value={compactNumber(s.total_cache_read_tokens)}
          note={percent(share(s.total_cache_read_tokens, billable))}
          delta={vs(s.total_cache_read_tokens, was?.total_cache_read_tokens)}
          title={`${integer(s.total_cache_read_tokens)} tokens`}
        />
        <Tile
          icon="database"
          label="Cache write"
          value={compactNumber(s.total_cache_creation_tokens)}
          note={percent(share(s.total_cache_creation_tokens, billable))}
          delta={vs(
            s.total_cache_creation_tokens,
            was?.total_cache_creation_tokens
          )}
          title={`${integer(s.total_cache_creation_tokens)} tokens`}
        />
        <Tile
          icon="activity"
          label="Avg / session"
          value={compactNumber(s.avg_tokens_per_session)}
          note="conversation tokens"
          delta={vs(s.avg_tokens_per_session, was?.avg_tokens_per_session)}
        />
        <Tile
          icon="dollar"
          label={unpriced ? "Est. cost (floor)" : "Est. cost"}
          value={usd(s.estimated_cost_usd)}
          tone="var(--green)"
          note={unpriced ? "excludes unpriced models" : "all token types"}
          delta={
            was === undefined ? undefined : costComparable ? (
              <Delta
                current={s.estimated_cost_usd}
                previous={was.estimated_cost_usd}
                format={usd}
              />
            ) : (
              <DeltaNote
                text="no cost delta"
                title="One of the two windows contains tokens with no published rate, so each total is a floor rather than a figure — the difference between two floors is not a change in spend."
              />
            )
          }
        />
      </div>

      {unpriced && <UnpricedNotice report={report} />}

      <Card
        title="Token composition"
        sub="Every token the estimated cost was computed over"
      >
        {billable > 0 ? (
          <>
            <div className="stackbar">
              {COMPOSITION.map((c) => {
                const v = s[c.field];
                return (
                  <span
                    key={c.key}
                    className="stackbar__seg"
                    style={{ width: widthPct(v, billable), background: c.color }}
                    title={`${c.key} — ${integer(v)} (${percent(share(v, billable))})`}
                  />
                );
              })}
            </div>
            <div className="legend">
              {COMPOSITION.map((c) => {
                const v = s[c.field];
                return (
                  <div key={c.key} className="legend__item">
                    <div className="legend__key">
                      <span
                        className="legend__swatch"
                        style={{ background: c.color }}
                      />
                      {c.key}
                    </div>
                    <div className="legend__val" title={`${integer(v)} tokens`}>
                      {compactNumber(v)}
                    </div>
                    <div className="legend__sub">
                      {percent(share(v, billable))} of billable
                    </div>
                  </div>
                );
              })}
            </div>
          </>
        ) : (
          <CardEmpty text="No tokens recorded in this period" />
        )}
      </Card>

      <div className="a-grid a-grid--2">
        <Card
          title="Tokens over time"
          badge={granularityLabel(report.granularity)}
        >
          <AreaChart
            points={tokenPoints}
            compare={comparePoints}
            format={compactNumber}
          />
        </Card>
        <Card
          title="Cache efficiency"
          sub="Share of input tokens served from cache"
        >
          <RateChart points={cachePoints} />
        </Card>
      </div>

      <Card title="Cost by model" table>
        {costs.length ? (
          <table className="table table--striped">
            <thead>
              <tr>
                <th>Model</th>
                <th>Provider</th>
                <th className="num">Sessions</th>
                <th className="num">Input</th>
                <th className="num">Output</th>
                <th className="num">Cache read</th>
                <th className="num">Cache write</th>
                <th className="num">Total</th>
                <th className="num">Share</th>
              </tr>
            </thead>
            <tbody>
              {costs.map((c) => (
                <tr key={`${c.provider}-${c.model}`}>
                  <td className="truncate" title={c.model}>
                    {c.model}
                  </td>
                  <td style={{ color: "var(--fg-tertiary)" }}>
                    {c.provider || "—"}
                  </td>
                  <td className="num">{integer(c.sessions)}</td>
                  <td className="num">{usd(c.cost.input_usd)}</td>
                  <td className="num">{usd(c.cost.output_usd)}</td>
                  <td className="num">{usd(c.cost.cache_read_usd)}</td>
                  <td className="num">{usd(c.cost.cache_write_usd)}</td>
                  <td className="num">{usd(c.cost.total_usd)}</td>
                  <td className="num">{percent(c.percentage)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : (
          <CardEmpty text="No priced model usage in this period" />
        )}
      </Card>

      <Card
        title="Projects"
        sub={foldedNote(projects)}
        table
      >
        {projects.length ? (
          <table className="table table--striped">
            <thead>
              <tr>
                <th>Project</th>
                <th className="num">Sessions</th>
                <th className="num">Conversation</th>
                <th className="num">All tokens</th>
                <th className="num">Cost</th>
                <th className="num">Share</th>
                <th>Last activity</th>
              </tr>
            </thead>
            <tbody>
              {projects.map((p) => {
                const folded = p.folded_projects ?? 0;
                return (
                  <tr key={p.project}>
                    <td className="truncate" title={p.project}>
                      {projectLabel(p.project)}
                      {folded > 0 && (
                        <span
                          className="badge"
                          style={{ marginLeft: "var(--sp-3)" }}
                          title="These projects were folded into one row by the server"
                        >
                          {folded} folded
                        </span>
                      )}
                    </td>
                    <td className="num">{integer(p.sessions)}</td>
                    <td className="num">
                      <Tokens n={p.tokens} />
                    </td>
                    <td className="num">
                      <Tokens n={p.total_tokens} />
                    </td>
                    <td className="num">{usd(p.cost.total_usd)}</td>
                    <td className="num">{percent(p.percentage)}</td>
                    <td style={{ color: "var(--fg-tertiary)" }}>
                      {p.last_activity
                        ? new Date(p.last_activity).toLocaleDateString(undefined, {
                            day: "numeric",
                            month: "short",
                          })
                        : "—"}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        ) : (
          <CardEmpty text="No project activity in this period" />
        )}
      </Card>

      <Insights cards={list(report.insight_cards)} />
    </>
  );
}

function foldedNote(
  projects: { project: string; folded_projects: number }[]
): string | undefined {
  const row = projects.find((p) => (p.folded_projects ?? 0) > 0);
  if (!row) return undefined;
  return `${row.folded_projects} smaller projects are folded into "${row.project}"`;
}

/**
 * Unpriced tokens make the total a floor, not a figure. Surfacing this is the
 * whole point — a silently-low cost is worse than no cost.
 */
function UnpricedNotice({ report }: { report: AnalyticsReport }) {
  const models = list(report.summary.unknown_pricing_models);
  return (
    <div className="banner">
      <Icon name="alert" size={14} />
      <span>
        {integer(report.summary.unknown_pricing_tokens)} tokens have no pricing
        rate, so {usd(report.summary.estimated_cost_usd)} is a{" "}
        <strong>floor</strong> rather than the real spend.
        {models.length > 0 && ` Unpriced: ${models.join(", ")}.`}
      </span>
    </div>
  );
}

/* --- Insight cards -------------------------------------------------------- */

function Insights({ cards }: { cards: InsightCard[] }) {
  if (!cards.length) return null;
  return (
    <div>
      <div
        className="card__title"
        style={{ marginBottom: "var(--sp-5)" }}
      >
        What the numbers suggest
      </div>
      <div className="a-insights">
        {cards.map((c, i) => (
          <InsightTile key={`${c.kind}-${i}`} card={c} />
        ))}
      </div>
    </div>
  );
}

interface Copy {
  icon: "layers" | "cpu" | "agent" | "dollar";
  heading: string;
  amount: string;
  text: React.ReactNode;
}

function copyFor(c: InsightCard): Copy {
  switch (c.kind) {
    case "cache_savings":
      return {
        icon: "layers",
        heading: "Cache savings",
        amount: usd(c.amount_usd),
        text: (
          <>
            {compactNumber(c.tokens)} tokens were served from cache. Billed at
            full input rates they would have cost {usd(c.amount_usd)} instead of
            the {usd(c.comparison_usd)} actually spent.
          </>
        ),
      };
    case "model_low_cache":
      return {
        icon: "cpu",
        heading: "Low cache reuse",
        amount: usd(c.amount_usd),
        text: (
          <>
            <strong>{c.model || "One model"}</strong> spent {usd(c.amount_usd)} (
            {percent(c.percent ?? 0)} of the period) over{" "}
            {compactNumber(c.tokens)} tokens with little cache reuse — the
            clearest place to cut cost.
          </>
        ),
      };
    case "delegation_mix":
      return {
        icon: "agent",
        heading: "Delegated work",
        amount: usd(c.amount_usd),
        text: (
          <>
            {integer(c.count ?? 0)} sessions ran on{" "}
            <strong>{c.model || "a delegated model"}</strong>, {percent(c.percent ?? 0)}{" "}
            of the period's spend.
          </>
        ),
      };
    case "expensive_sessions":
      return {
        icon: "dollar",
        heading: "Heaviest sessions",
        amount: usd(c.amount_usd),
        text: (
          <>
            {integer(c.count ?? 0)} sessions account for {percent(c.percent ?? 0)} of
            all spend, averaging {duration(c.avg_duration_ms)} each.
          </>
        ),
      };
  }
}

function InsightTile({ card }: { card: InsightCard }) {
  const copy = copyFor(card);
  return (
    <div className="a-insight">
      <div className="a-insight__head">
        <Icon name={copy.icon} size={13} />
        {copy.heading}
        {card.estimated && (
          <>
            <div className="spacer" />
            <span
              className="badge badge--amber"
              title="Modelled from list prices, not billed amounts"
            >
              Estimate
            </span>
          </>
        )}
      </div>
      <div className="a-insight__amount">{copy.amount}</div>
      <div className="a-insight__text">{copy.text}</div>
    </div>
  );
}
