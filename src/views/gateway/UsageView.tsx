import { useMemo, useState } from "react";
import { api, qs } from "../../lib/api";
import { AreaChart, BarChart, CardEmpty, type Point } from "../../components/charts";
import { Empty, Segmented, Splitter } from "../../components/ui";
import { Icon, type IconName } from "../../lib/icons";
import { compactNumber, integer, latency, percent, usd } from "../../lib/format";
import { useResource } from "../../lib/hooks";
import type {
  GatewaySettings,
  GatewayUsage,
  GatewayUsageGroup,
} from "../../lib/types";
import "../../styles/gateway.css";

/* ============================================================================
   LLM Gateway → Usage (#428).

   What the gateway spent, over `GET /api/gateway/usage`. Structurally this is
   `AnalyticsView`'s dashboard — a period selector, one windowed read, tiles,
   charts, ranked breakdowns — and it shares that view's *chart components* and
   nothing else. No `stats.ts`, no Claude wire type, no analytics endpoint: the
   epic's locked decision is that gateway traffic is never mixed into Claude's
   own figures, and this view is the visible half of it.

   Three numbers here are easy to render wrongly, and each is guarded at the
   place it is computed rather than at the encoder:

   - **Cost is a floor.** An unpriced request stores `NULL`, not `0.0`, because
     the pricing catalog is seeded for Claude models and OpenAI/Gemini/GLM
     aliases miss routinely. Whenever `unpriced_requests > 0` the total is
     labelled and the models are listed. `$0.00` never stands in for "not
     priced".
   - **A pruned window is a floor too.** Retention deletes past the horizon, so
     a window reaching further back than `usage_retention_days` under-reports —
     and an under-reported total that looks complete is exactly the failure a
     prune introduces silently. Same wording as the unpriced note, so the two
     read as one idea.
   - **Every ratio guards its own division.** `share()` answers 0 for a zero
     total; a bare `part / total` would reach the tile as `NaN`, which Go's
     encoder refuses outright and `serde_json` renders as `null` — a wrong
     number, not an error.
   ========================================================================== */

type RangeKey = "7d" | "30d" | "90d";

const RANGE_OPTIONS: { value: RangeKey; label: string }[] = [
  { value: "7d", label: "7d" },
  { value: "30d", label: "30d" },
  { value: "90d", label: "90d" },
];

/**
 * The server buckets in this zone. Omitting it silently falls the whole
 * dashboard back to UTC, which shifts every bucket — so it is sent on every
 * request without exception, exactly as `AnalyticsView` does.
 */
const TZ = Intl.DateTimeFormat().resolvedOptions().timeZone;

const DAY_MS = 86_400_000;

interface Period {
  from: string;
  to: string;
  days: number;
  label: string;
}

function computePeriod(key: RangeKey): Period {
  const days = key === "7d" ? 7 : key === "30d" ? 30 : 90;
  const start = new Date();
  start.setHours(0, 0, 0, 0);
  const endExclusive = new Date(start.getTime() + DAY_MS);
  return {
    from: new Date(endExclusive.getTime() - days * DAY_MS).toISOString(),
    to: endExclusive.toISOString(),
    days,
    label: `Last ${days} days`,
  };
}

/** part/total as a percentage, 0 when the total is zero or non-finite. */
function share(part: number, total: number): number {
  if (!isFinite(part) || !isFinite(total) || total <= 0) return 0;
  const pct = (part / total) * 100;
  return isFinite(pct) ? pct : 0;
}

function widthPct(part: number, total: number): string {
  return `${Math.min(100, Math.max(0, share(part, total)))}%`;
}

/** Go marshals an empty slice as null, so every array arrives possibly-null. */
function list<T>(v: T[] | null | undefined): T[] {
  return v ?? [];
}

const MONTHS = [
  "Jan", "Feb", "Mar", "Apr", "May", "Jun",
  "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/**
 * Bucket keys are `2026-08-13`, or `2026-08-13T14` when hourly, and are already
 * in the requested zone — so they are formatted by string surgery rather than
 * through `new Date`, which would read `2026-08-13` as UTC midnight and render
 * the previous day for any viewer west of Greenwich.
 */
function bucketLabel(date: string, granularity: GatewayUsage["granularity"]): string {
  const [day, hour] = date.split("T");
  const [y, m, d] = day.split("-");
  const month = MONTHS[Number(m) - 1] ?? m;
  switch (granularity) {
    case "hourly":
      return `${hour ?? "00"}:00`;
    case "monthly":
      return `${month} ${y}`;
    case "yearly":
      return y;
    default:
      return `${Number(d)} ${month}`;
  }
}

/** How each stored status reads, and which tone says so. */
const STATUS_COPY: Record<string, { label: string; tone: string }> = {
  ok: { label: "Served", tone: "badge--green" },
  upstream_error: { label: "Upstream error", tone: "badge--red" },
  // Kept apart from `upstream_error` on purpose (#425): a client that went away
  // is not a provider that failed, and merging them would make a closed tab
  // read as an outage.
  interrupted: { label: "Client left", tone: "badge--amber" },
  refused: { label: "Refused", tone: "badge--purple" },
};

export function GatewayUsageView({ inspectorOpen }: { inspectorOpen: boolean }) {
  const [range, setRange] = useState<RangeKey>("30d");
  const period = useMemo(() => computePeriod(range), [range]);

  const query = qs({ from: period.from, to: period.to, tz: TZ });
  const usage = useResource<GatewayUsage>(
    (signal) => api.get(`/gateway/usage${query}`, signal),
    [query]
  );

  // Read only for the retention horizon, which is the Settings view's to write.
  // A failure here must not hide the dashboard — it costs the disclosure line
  // and nothing else — so it is surfaced beside the usage error rather than
  // gating the render.
  const settings = useResource<GatewaySettings>(
    (signal) => api.get("/gateway/settings", signal),
    []
  );

  const data = usage.data;
  const hasData = data !== undefined;

  const retentionDays = settings.data?.usage_retention_days ?? 0;
  // `0` keeps everything, so no window can predate the horizon.
  const windowPredatesRetention =
    retentionDays > 0 && period.days > retentionDays;

  return (
    <div className="panes">
      <div className="pane-detail">
        <div className="toolbar">
          <div className="toolbar__title">Usage</div>
          <div className="toolbar__sep" />
          <Segmented<RangeKey>
            value={range}
            onChange={setRange}
            options={RANGE_OPTIONS}
          />
          <div className="spacer" />
          <span className="toolbar__sub tnum">
            {hasData
              ? `${period.label} · ${integer(data.totals.requests)} requests`
              : period.label}
          </span>
          <div className="toolbar__sep" />
          <button
            className="iconbtn"
            title="Refresh"
            onClick={() => {
              usage.reload();
              settings.reload();
            }}
          >
            <Icon name="refresh" size={14} />
          </button>
        </div>

        {/* Keyed by which read failed rather than by the message: two requests
            refused for the same reason carry identical text, and a view that
            renders only one of them hides the other as an empty result. */}
        {(
          [
            ["usage", usage.error],
            ["settings", settings.error],
          ] as const
        ).map(([source, message]) =>
          message && !(source === "usage" && !hasData) ? (
            <div className="banner gw-usage__banner" key={source}>
              <Icon name="alert" size={14} />
              <span>
                {source === "usage"
                  ? `Refresh failed — showing the last good data. ${message}`
                  : `The retention horizon could not be read, so a shortened window is not disclosed below. ${message}`}
              </span>
            </div>
          ) : null
        )}

        {usage.error && !hasData ? (
          <Empty
            icon="alert"
            title="Could not load gateway usage"
            text={usage.error}
            action={
              <button className="btn" onClick={() => usage.reload()}>
                Try again
              </button>
            }
          />
        ) : !hasData ? (
          <div className="gw-usage__state">
            {usage.loading ? "Loading usage…" : "No data"}
          </div>
        ) : (
          <div
            className={`dash scroll gw-usage ${usage.loading ? "gw-usage--stale" : ""}`}
          >
            <Body
              usage={data}
              period={period}
              retentionDays={retentionDays}
              windowPredatesRetention={windowPredatesRetention}
            />
          </div>
        )}
      </div>

      {inspectorOpen && (
        <>
          <Splitter variable="--inspector-w" min={220} max={420} invert />
          <aside className="pane-inspector">
            <div className="inspector__head">Gateway Usage</div>
            <div className="inspector__scroll scroll">
              <div className="insp-group">
                <div className="insp-group__title">What this counts</div>
                <p className="gw-help">
                  One row per request the gateway served, on your own provider
                  credentials. It shares nothing with the Claude Usage and
                  Analytics sections above — those report on Claude Code runs,
                  which are billed separately.
                </p>
              </div>
              <div className="insp-group">
                <div className="insp-group__title">Why a total can be a floor</div>
                <p className="gw-help">
                  Cost is priced when a request is recorded, against the built-in
                  catalog. A model the catalog does not price is counted and
                  named rather than charged at zero, so the total is the least it
                  can have been.
                </p>
                <p className="gw-help gw-help--muted">
                  {retentionDays > 0
                    ? `Rows are kept for ${retentionDays} day${retentionDays === 1 ? "" : "s"}; a window reaching further back is a floor for the same reason. Change it in Gateway Settings.`
                    : "Rows are kept indefinitely — the retention horizon is set to keep everything."}
                </p>
              </div>
            </div>
          </aside>
        </>
      )}
    </div>
  );
}

function Body({
  usage,
  period,
  retentionDays,
  windowPredatesRetention,
}: {
  usage: GatewayUsage;
  period: Period;
  retentionDays: number;
  windowPredatesRetention: boolean;
}) {
  const t = usage.totals;
  const g = usage.granularity;
  const series = list(usage.series);

  const totalTokens =
    (t.prompt_tokens ?? 0) +
    (t.completion_tokens ?? 0) +
    (t.cache_read_tokens ?? 0) +
    (t.cache_write_tokens ?? 0);

  const requestPoints: Point[] = series.map((p) => ({
    label: bucketLabel(p.date, g),
    value: p.requests ?? 0,
    hint: `${bucketLabel(p.date, g)} — ${p.requests ?? 0} request${
      p.requests === 1 ? "" : "s"
    } · ${compactNumber(p.prompt_tokens + p.completion_tokens)} tokens`,
  }));

  const tokenPoints: Point[] = series.map((p) => ({
    label: bucketLabel(p.date, g),
    value: (p.prompt_tokens ?? 0) + (p.completion_tokens ?? 0),
    hint: `${bucketLabel(p.date, g)} — ${compactNumber(p.prompt_tokens)} in · ${compactNumber(p.completion_tokens)} out`,
  }));

  const costPoints: Point[] = series.map((p) => ({
    label: bucketLabel(p.date, g),
    value: p.cost_usd ?? 0,
    hint: `${bucketLabel(p.date, g)} — ${usd(p.cost_usd)}`,
  }));

  const statuses = list(usage.by_status);
  const failures = statuses
    .filter((s) => s.key === "upstream_error" || s.key === "refused")
    .reduce((sum, s) => sum + (s.requests ?? 0), 0);
  const errorRate = share(failures, t.requests ?? 0);

  const unpricedModels = list(t.unpriced_models);
  const costIsFloor = (t.unpriced_requests ?? 0) > 0;

  return (
    <>
      <div className="tiles">
        <Tile icon="zap" label="Requests" value={integer(t.requests)} />
        <Tile
          icon="chart"
          label="Tokens"
          value={compactNumber(totalTokens)}
          note={`${compactNumber(t.prompt_tokens)} in · ${compactNumber(t.completion_tokens)} out`}
          title={`${integer(totalTokens)} tokens, cache included`}
        />
        <Tile
          icon="database"
          label={costIsFloor ? "Cost (at least)" : "Cost"}
          value={usd(t.cost_usd)}
          note={
            costIsFloor
              ? `${integer(t.unpriced_requests)} request${t.unpriced_requests === 1 ? "" : "s"} not priced`
              : undefined
          }
        />
        <Tile
          icon="alert"
          label="Error rate"
          value={percent(errorRate)}
          note={
            (t.requests ?? 0) > 0
              ? `${integer(failures)} of ${integer(t.requests)}`
              : "no traffic"
          }
          tone={errorRate > 0 ? "var(--red)" : undefined}
        />
        <Tile
          icon="clock"
          label="Latency p95"
          value={(t.requests ?? 0) > 0 ? latency(usage.latency.p95_ms) : "—"}
          note={
            (t.requests ?? 0) > 0
              ? `p50 ${latency(usage.latency.p50_ms)} · max ${latency(usage.latency.max_ms)}`
              : undefined
          }
        />
      </div>

      {(costIsFloor || windowPredatesRetention) && (
        <div className="gw-usage__floor">
          <Icon name="alert" size={13} />
          <div>
            {costIsFloor && (
              <p className="gw-help">
                The cost above is a <strong>floor</strong>:{" "}
                {integer(t.unpriced_requests)} request
                {t.unpriced_requests === 1 ? "" : "s"} used a model the built-in
                pricing catalog does not price, and unpriced traffic is disclosed
                rather than charged at zero
                {unpricedModels.length > 0 ? " — " : "."}
                {unpricedModels.length > 0 && (
                  <span className="mono">{unpricedModels.join(", ")}</span>
                )}
              </p>
            )}
            {windowPredatesRetention && (
              <p className="gw-help">
                Every total above is a <strong>floor</strong> for a second
                reason: this window reaches back {period.days} days and usage
                rows are kept for {retentionDays}, so anything older has already
                been pruned. Raise the horizon in Gateway Settings to keep more.
              </p>
            )}
          </div>
        </div>
      )}

      <div className="gw-usage__grid gw-usage__grid--wide">
        <Card title="Requests over time" badge={granularityLabel(g)}>
          <AreaChart points={requestPoints} format={integer} />
        </Card>
        <Card title="Outcome" table>
          <Breakdown
            rows={statuses}
            total={t.requests ?? 0}
            label={(key) => STATUS_COPY[key]?.label ?? (key || "—")}
            badge={(key) => STATUS_COPY[key]?.tone}
            emptyText="Nothing served in this period"
          />
        </Card>
      </div>

      <div className="gw-usage__grid gw-usage__grid--2">
        <Card title="Tokens over time" sub="Prompt plus completion">
          <BarChart points={tokenPoints} format={compactNumber} />
        </Card>
        <Card title="Spend over time" sub="Priced requests only">
          <BarChart points={costPoints} format={usd} tone="var(--teal)" />
        </Card>
      </div>

      <div className="gw-usage__grid gw-usage__grid--2">
        <Card title="By model alias" sub="What your tools asked for" table>
          <Breakdown
            rows={list(usage.by_alias)}
            total={t.requests ?? 0}
            emptyText="No aliases served in this period"
          />
        </Card>
        <Card title="By provider" sub="Where it was actually served" table>
          <Breakdown
            rows={list(usage.by_provider)}
            total={t.requests ?? 0}
            emptyText="No providers served in this period"
          />
        </Card>
      </div>

      <div className="gw-usage__grid gw-usage__grid--2">
        <Card title="By surface" sub="Which wire format the client spoke" table>
          <Breakdown
            rows={list(usage.by_surface)}
            total={t.requests ?? 0}
            emptyText="No traffic in this period"
          />
        </Card>
        <Card title="By token" sub="Which credential spent it" table>
          {/* The server resolves each key to the token's name; the raw
              `api_tokens` id is the fallback for a token deleted outright, and
              is shown rather than hidden so the row still accounts for its
              spend. `""` is a request no token could be attributed to. */}
          <Breakdown
            rows={list(usage.by_token)}
            total={t.requests ?? 0}
            label={(key, row) => row.label || key || "unattributed"}
            emptyText="No traffic in this period"
          />
        </Card>
      </div>
    </>
  );
}

/**
 * A ranked breakdown with a share bar, cost and tokens.
 *
 * Gateway-owned rather than reusing `analytics/shared.tsx`'s `RankList`: that
 * one takes a Claude `TopEntry`, and widening it to a second wire type is
 * precisely the coupling the locked decision rules out. The chart *components*
 * are shared; the Claude-typed helpers around them are not.
 */
function Breakdown({
  rows,
  total,
  emptyText,
  label = (key) => key || "—",
  badge,
}: {
  rows: GatewayUsageGroup[];
  total: number;
  emptyText: string;
  /** Takes the row too, so `by_token` can prefer the server-resolved name. */
  label?: (key: string, row: GatewayUsageGroup) => string;
  badge?: (key: string) => string | undefined;
}) {
  if (!rows.length) return <CardEmpty text={emptyText} />;

  // The server groups into a BTreeMap, so rows arrive in key order — useful for
  // a stable render, useless for a ranked list. Sorted by what the bar measures.
  const ranked = [...rows].sort((a, b) => (b.requests ?? 0) - (a.requests ?? 0));

  return (
    <div className="gw-rank">
      {ranked.map((row) => {
        const text = label(row.key, row);
        const tone = badge?.(row.key);
        // Token names are not unique — `api_tokens` has no constraint on `name`
        // and the create route only checks it is non-empty — so two credentials
        // called "Zed" would otherwise be two indistinguishable rows in the one
        // panel whose job is picking which of them to revoke. The id
        // disambiguates on hover without putting a UUID on screen.
        const hint = row.label && row.label !== row.key ? `${text} · ${row.key}` : text;
        return (
          <div className="gw-rank__row" key={row.key}>
            <div className="gw-rank__name">
              <div className="truncate" title={hint}>
                {tone ? <span className={`badge ${tone}`}>{text}</span> : text}
              </div>
              <div className="gw-rank__bar">
                <div
                  className="gw-rank__fill"
                  style={{ width: widthPct(row.requests ?? 0, total) }}
                />
              </div>
            </div>
            <div className="gw-rank__count tnum">
              {integer(row.requests)}
              <span style={{ color: "var(--fg-quaternary)" }}>
                {" "}
                {percent(share(row.requests ?? 0, total), 0)}
              </span>
              <div className="gw-rank__sub">
                {usd(row.cost_usd)} ·{" "}
                {compactNumber(
                  (row.prompt_tokens ?? 0) + (row.completion_tokens ?? 0)
                )}{" "}
                tok
              </div>
            </div>
          </div>
        );
      })}
    </div>
  );
}

function granularityLabel(g: GatewayUsage["granularity"]): string {
  return g.charAt(0).toUpperCase() + g.slice(1);
}

function Tile({
  icon,
  label,
  value,
  note,
  tone,
  title,
}: {
  icon: IconName;
  label: string;
  value: string;
  note?: string;
  tone?: string;
  title?: string;
}) {
  return (
    <div className="tile" title={title}>
      <div className="tile__label">
        <Icon name={icon} size={13} />
        {label}
      </div>
      <div className="tile__value" style={tone ? { color: tone } : undefined}>
        {value}
      </div>
      {note && (
        <div className="tile__delta" style={{ color: "var(--fg-quaternary)" }}>
          {note}
        </div>
      )}
    </div>
  );
}

function Card({
  title,
  sub,
  badge,
  table,
  children,
}: {
  title: string;
  sub?: string;
  badge?: string;
  table?: boolean;
  children: React.ReactNode;
}) {
  return (
    <div className={`card ${table ? "gw-usage__card--table" : ""}`}>
      <div className="card__head">
        <div className="card__title">{title}</div>
        {sub && <span className="toolbar__sub truncate">{sub}</span>}
        {badge && (
          <>
            <div className="spacer" />
            <span className="badge badge--accent">{badge}</span>
          </>
        )}
      </div>
      <div className="card__body">{children}</div>
    </div>
  );
}
