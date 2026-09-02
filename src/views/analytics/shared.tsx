import { Icon, type IconName } from "../../lib/icons";
import { CardEmpty } from "../../components/charts";
import { compactNumber, integer, percent, tildePath } from "../../lib/format";
import type { AnalyticsReport, TopEntry } from "../../lib/types";
import type { Eq, Expect } from "../../lib/typeAssert";

/**
 * `CardEmpty` moved to `components/charts.tsx` with the charts that fall back to
 * it (#428) and is re-exported here so no analytics call site had to churn — the
 * chart lift is a move, and a move that rewrites five import lists is harder to
 * read as one.
 */
export { CardEmpty };

/* ============================================================================
   Shared analytics plumbing: the period model, safe arithmetic, bucket labels
   and the small presentational primitives every mode reuses.
   ========================================================================== */

export type Granularity = AnalyticsReport["granularity"];

export type RangeKey = "7d" | "30d" | "90d" | "all" | "custom";

export interface Period {
  /** RFC3339. Undefined only if a custom range is half-filled. */
  from?: string;
  to?: string;
  /** Whole days covered, for the inspector. 0 when unknown. */
  days: number;
  label: string;
}

/**
 * The server buckets in this zone. Omitting it silently falls the whole
 * dashboard back to UTC, which shifts every date bucket and the hourly
 * histogram — so it is sent on every analytics request without exception.
 */
export const TZ = Intl.DateTimeFormat().resolvedOptions().timeZone;

/**
 * "All time" still needs an explicit `from`: with no range the server defaults
 * to the last 31 days. This floor predates Claude Code itself, so it covers
 * everything while keeping the server on weekly (not yearly) buckets.
 */
const ALL_TIME_FLOOR = "2024-01-01T00:00:00.000Z";

const DAY_MS = 86_400_000;

function startOfLocalDay(d: Date): Date {
  const copy = new Date(d);
  copy.setHours(0, 0, 0, 0);
  return copy;
}

/** Parse a `<input type="date">` value as a *local* day, not a UTC instant. */
function parseLocalDate(value: string): Date | undefined {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (!m) return undefined;
  const d = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  return isFinite(d.getTime()) ? d : undefined;
}

export function computePeriod(
  key: RangeKey,
  customFrom: string,
  customTo: string
): Period {
  const now = new Date();
  const endExclusive = new Date(startOfLocalDay(now).getTime() + DAY_MS);

  if (key === "custom") {
    const f = parseLocalDate(customFrom);
    const t = parseLocalDate(customTo);
    if (!f || !t) return { days: 0, label: "Custom range — pick both dates" };
    // The picker means "through this day", so the window ends at its midnight.
    const to = new Date(t.getTime() + DAY_MS);
    if (to.getTime() <= f.getTime()) {
      return { days: 0, label: "Custom range — end is before start" };
    }
    const days = Math.round((to.getTime() - f.getTime()) / DAY_MS);
    return {
      from: f.toISOString(),
      to: to.toISOString(),
      days,
      label: `${customFrom} → ${customTo}`,
    };
  }

  if (key === "all") {
    return {
      from: ALL_TIME_FLOOR,
      to: endExclusive.toISOString(),
      days: 0,
      label: "All time",
    };
  }

  const days = key === "7d" ? 7 : key === "30d" ? 30 : 90;
  const from = new Date(endExclusive.getTime() - days * DAY_MS);
  return {
    from: from.toISOString(),
    to: endExclusive.toISOString(),
    days,
    label: `Last ${days} days`,
  };
}

/**
 * The window immediately before `p`, of exactly the same length (#539).
 *
 * `undefined` when there is no well-defined predecessor: **All time**, whose
 * window is an arbitrary floor rather than a duration anybody chose, and a
 * half-filled or inverted custom range, which has no bounds to shift.
 *
 * Two things about the shape:
 *
 * * **It takes the `RangeKey`, not just the `Period`.** A `Period` does not
 *   carry which preset produced it — `days` is 0 for All time *and* for an
 *   invalid custom range — and the only other discriminator is `label`, which
 *   is rendered text. Keying behaviour on rendered text is exactly what
 *   `lib/inspectorPrefs.ts` exists to warn about.
 * * **It shifts epoch milliseconds, never calendar units.** `to − from` is the
 *   window's real length including any DST transition inside it, so the shifted
 *   window is the same number of *hours* as the one it is compared with. A
 *   calendar shift ("the previous 30 days" as `AddDate(0,0,-30)`) would produce
 *   a window an hour longer or shorter twice a year.
 *
 * The current window's `from` is the shifted window's exclusive end, matching
 * the convention `computePeriod` already uses for `to`.
 */
export function previousPeriod(key: RangeKey, p: Period): Period | undefined {
  if (key === "all") return undefined;
  if (!p.from || !p.to) return undefined;

  const from = Date.parse(p.from);
  const to = Date.parse(p.to);
  if (!isFinite(from) || !isFinite(to) || to <= from) return undefined;

  const len = to - from;
  const days = Math.round(len / DAY_MS);
  return {
    from: new Date(from - len).toISOString(),
    to: new Date(from).toISOString(),
    days,
    label: days > 0 ? `Previous ${days} days` : "Previous period",
  };
}

export const RANGE_OPTIONS: { value: RangeKey; label: string }[] = [
  { value: "7d", label: "7d" },
  { value: "30d", label: "30d" },
  { value: "90d", label: "90d" },
  { value: "all", label: "All" },
  { value: "custom", label: "Custom" },
];

/* --- Safe arithmetic ----------------------------------------------------- */

/** part/total as a percentage, 0 when the total is zero or non-finite. */
export function share(part: number, total: number): number {
  if (!isFinite(part) || !isFinite(total) || total <= 0) return 0;
  const pct = (part / total) * 100;
  return isFinite(pct) ? pct : 0;
}

/** Clamp to [0,100] for anything driving a CSS width. */
export function widthPct(part: number, total: number): string {
  return `${Math.min(100, Math.max(0, share(part, total)))}%`;
}

/** Go marshals an empty slice as null, so every array arrives possibly-null. */
export function list<T>(v: T[] | null | undefined): T[] {
  return v ?? [];
}

export interface Delta {
  /** Signed percentage change against the previous window. */
  pct: number;
  dir: "up" | "down" | "flat";
}

/**
 * The direction spelling, pinned (`lib/typeAssert.ts`).
 *
 * `Delta.dir` is this module's public surface — it chooses the glyph, the sign
 * and the optional colour class — and there is no TypeScript test harness here,
 * so an added or respelled variant is caught by `tsc` or by nothing.
 */
export type PinDeltaDirections = Expect<Eq<Delta["dir"], "up" | "down" | "flat">>;

/**
 * `current` against `previous`, as a signed percentage (#539).
 *
 * `undefined` when there is no baseline to divide by — a previous value of zero
 * or below, or either side non-finite. That is a *render* decision rather than a
 * number: "+∞%" and "NaN%" are both worse than saying there was nothing to
 * compare against, and the caller knows which of the two no-baseline cases it is
 * in (nothing then and something now, or nothing either way).
 *
 * `flat` is anything that rounds to `0.0%` at `percent()`'s own precision, so
 * the arrow never disagrees with the figure beside it.
 */
export function delta(current: number, previous: number): Delta | undefined {
  if (!isFinite(current) || !isFinite(previous) || previous <= 0) return undefined;
  const pct = ((current - previous) / previous) * 100;
  if (!isFinite(pct)) return undefined;
  return { pct, dir: pct >= 0.05 ? "up" : pct <= -0.05 ? "down" : "flat" };
}

/** `{ tool }` on every endpoint checked, but the wire type allows `name`. */
export function entryLabel(e: TopEntry): string {
  return e.tool ?? e.name ?? "—";
}

/* --- Bucket labels ------------------------------------------------------- */

const MONTHS = [
  "Jan", "Feb", "Mar", "Apr", "May", "Jun",
  "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/**
 * Bucket keys are `2026-08-13`, or `2026-08-13T14` when hourly. They are
 * already in the requested timezone, so they are formatted by string surgery:
 * `new Date("2026-08-13")` is UTC midnight and would render as the previous
 * day for any viewer west of Greenwich.
 */
export function bucketLabel(date: string, granularity: Granularity): string {
  const [day, hour] = date.split("T");
  const [y, m, d] = day.split("-");
  const month = MONTHS[Number(m) - 1] ?? m;

  switch (granularity) {
    case "hourly":
      return `${hour ?? "00"}:00`;
    case "weekly":
      return `${Number(d)} ${month}`;
    case "monthly":
      return `${month} ${y}`;
    case "yearly":
      return y;
    default:
      return `${Number(d)} ${month}`;
  }
}

export function granularityLabel(g: Granularity): string {
  return g.charAt(0).toUpperCase() + g.slice(1);
}

/** The span `trimEmpty` keeps, as `[start, end)` into the original array. */
function trimBounds<T>(
  points: T[],
  value: (p: T) => number
): { start: number; end: number } {
  let start = 0;
  let end = points.length;
  while (start < end && value(points[start]) <= 0) start++;
  while (end > start && value(points[end - 1]) <= 0) end--;
  return { start, end };
}

/**
 * Trim all-zero buckets from both ends. "All time" asks for a window far wider
 * than the data, and without this the real activity is squeezed into a corner.
 * Interior zeros are kept — a quiet week is signal.
 */
export function trimEmpty<T>(points: T[], value: (p: T) => number): T[] {
  const { start, end } = trimBounds(points, value);
  return start === 0 && end === points.length ? points : points.slice(start, end);
}

/**
 * The trimmed current series and the comparison series windowed to match it
 * (#539).
 *
 * **Trimming the comparison on its own rule is the bug this exists to
 * prevent**, and it is not obvious: the rule really is the same, but the
 * *result* is data-dependent, and it is the result the overlay's alignment
 * depends on. `AreaChart` lines the two series up from their last elements, on
 * the reasoning that the server emits every bucket from `from` to `to`
 * inclusive so the last element of each is the last bucket of its window. Trim
 * each independently and that stops being true: a previous window that went
 * quiet for its last ten days loses ten trailing buckets the current one keeps,
 * and its busiest stretch is drawn under the current window's most recent days
 * — with the tooltip stating the pairing as fact.
 *
 * So the comparison is windowed by *position within its own window*, never by
 * its own zeros. Both arrays are first aligned at their ends — equal-duration
 * windows can still hold different bucket counts across a DST transition or
 * unequal month lengths — and then the current series' `[start, end)` is applied
 * to both. Positions with no counterpart can only be a leading run (the shift is
 * monotonic and the end is aligned), so what comes back is contiguous and still
 * end-aligned; `AreaChart`'s own truncation stays the residual safety net it was
 * written to be.
 *
 * **Both series come back from one call because they must share one `value`
 * predicate.** Two calls is two chances to pass different ones, and a
 * disagreement there misaligns the overlay silently — which is the failure this
 * function is for.
 *
 * **What comes back is a positional subset, so never derive a whole-window
 * aggregate from it.** `trimEmpty` only ever removes zero buckets, so a count or
 * an average over the current series is still the whole window's; `alignedPair`
 * drops *non-zero* comparison buckets wherever the current window was idle, so
 * the same count over the comparison is not. Aggregate over
 * `previous.time_series` itself.
 */
export function alignedPair<T, U>(
  current: T[],
  previous: U[] | undefined,
  value: (p: T) => number
): { series: T[]; compare: U[] | undefined } {
  const { start, end } = trimBounds(current, value);
  const series =
    start === 0 && end === current.length ? current : current.slice(start, end);
  if (!previous) return { series, compare: undefined };

  const shift = current.length - previous.length;
  const compare: U[] = [];
  for (let i = start; i < end; i++) {
    const j = i - shift;
    if (j >= 0 && j < previous.length) compare.push(previous[j]);
  }
  return { series, compare };
}

/** Project paths are absolute on the wire; folded/unresolved ones are not. */
export function projectLabel(project: string): string {
  return project.startsWith("/") ? tildePath(project) : project;
}

/* --- Presentational primitives ------------------------------------------- */

export function Tile({
  icon,
  label,
  value,
  note,
  delta,
  tone,
  title,
}: {
  icon: IconName;
  label: string;
  value: string;
  note?: string;
  /**
   * The comparison annotation, which *replaces* the note when present (#539).
   * One slot, because there is one line under the value — a caller wanting both
   * would be asking the tile to grow, which is a different change.
   *
   * A caller passing nothing takes the note branch and emits exactly the markup
   * it always did, which is what keeps the toggle-off rendering byte-identical.
   */
  delta?: React.ReactNode;
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
      {/* `||`, not `??`: a call site that spells "no delta" as `x && <Delta/>`
          yields `false` rather than `undefined`, and that must still fall
          through to the note rather than blanking the line. */}
      {delta ||
        (note && (
          <div className="tile__delta" style={{ color: "var(--fg-quaternary)" }}>
            {note}
          </div>
        ))}
    </div>
  );
}

/**
 * A tile's period-over-period change (#539).
 *
 * **Neutral by default, and deliberately so.** `.tile__delta--up` /
 * `--down` are green and red, and that polarity is wrong for most of what this
 * dashboard reports: spending 20% more is not "good", and using 20% fewer
 * tokens is not "bad" — both are just what happened. So the direction is carried
 * by an arrow and a signed figure, and colour is left to the metrics whose
 * polarity the product itself states. None of the tiles in Tokens or Usage mode
 * is one, so nothing sets `polarity` today; the prop exists because the *first*
 * such tile must not have to re-argue this at its call site.
 *
 * The previous absolute value goes in the `title`, because a percentage with no
 * baseline is unreadable: "+400%" over two sessions and over two thousand are
 * very different facts.
 */
export function Delta({
  current,
  previous,
  format = compactNumber,
  polarity = "neutral",
}: {
  current: number;
  previous: number;
  /** How the previous absolute value is spelled in the tooltip. */
  format?: (n: number) => string;
  /** `up-is-good` colours a rise green; `down-is-good` colours a fall green. */
  polarity?: "neutral" | "up-is-good" | "down-is-good";
}) {
  const d = delta(current, previous);
  const was = `Previous period: ${format(previous)}`;

  // No baseline. "New" and "—" are different facts and only the caller's data
  // can tell them apart, so both are spelled here rather than collapsed.
  if (!d) {
    return (
      <div className="tile__delta" style={{ color: "var(--fg-quaternary)" }} title={was}>
        {previous <= 0 && current > 0 ? "new — no baseline" : "no baseline"}
      </div>
    );
  }

  const good =
    polarity === "neutral" || d.dir === "flat"
      ? undefined
      : (polarity === "up-is-good") === (d.dir === "up");
  const tone =
    good === undefined ? "" : good ? " tile__delta--up" : " tile__delta--down";

  // `flat` gets its own glyph and no sign: an up arrow over "0.0%" reads as a
  // rise the figure beside it does not show.
  const icon = d.dir === "up" ? "arrowUp" : d.dir === "down" ? "arrowDown" : "minus";

  return (
    <div className={`tile__delta${tone}`} title={was}>
      <Icon name={icon} size={12} />
      <span className="tnum">
        {d.dir === "up" ? "+" : ""}
        {percent(d.pct)}
      </span>
    </div>
  );
}

/**
 * The delta slot carrying a sentence instead of a figure — used where a
 * comparison exists but would not be honest, which today is the cost tile
 * against a window holding unpriced tokens (#539).
 */
export function DeltaNote({ text, title }: { text: string; title?: string }) {
  return (
    <div className="tile__delta" style={{ color: "var(--amber)" }} title={title}>
      <Icon name="alert" size={12} />
      <span>{text}</span>
    </div>
  );
}

export function Card({
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
    <div className={`card ${table ? "a-card--table" : ""}`}>
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

/** A ranked `{label, count}` list with a share bar. Used all over insights. */
export function RankList({
  entries,
  emptyText,
  format = integer,
}: {
  entries: TopEntry[] | null | undefined;
  emptyText: string;
  format?: (n: number) => string;
}) {
  const rows = list(entries);
  if (!rows.length) return <CardEmpty text={emptyText} />;

  const max = rows.reduce((m, e) => Math.max(m, e.count), 0);

  return (
    <div className="a-rank">
      {rows.map((e, i) => {
        const label = entryLabel(e);
        return (
          <div className="a-rank__row" key={`${label}-${i}`}>
            <div className="a-rank__name">
              <div className="truncate" title={label}>
                {label}
              </div>
              <div className="a-rank__bar">
                <div
                  className="a-rank__fill"
                  style={{ width: widthPct(e.count, max) }}
                />
              </div>
            </div>
            <div className="a-rank__count">{format(e.count)}</div>
          </div>
        );
      })}
    </div>
  );
}

/** Token counts are large enough that the exact figure belongs in a tooltip. */
export function Tokens({ n }: { n: number }) {
  return (
    <span className="tnum" title={`${integer(n)} tokens`}>
      {compactNumber(n)}
    </span>
  );
}
