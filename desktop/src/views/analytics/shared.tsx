import { Icon, type IconName } from "../../lib/icons";
import { compactNumber, integer, tildePath } from "../../lib/format";
import type { AnalyticsReport, TopEntry } from "../../lib/types";

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

/**
 * Trim all-zero buckets from both ends. "All time" asks for a window far wider
 * than the data, and without this the real activity is squeezed into a corner.
 * Interior zeros are kept — a quiet week is signal.
 */
export function trimEmpty<T>(points: T[], value: (p: T) => number): T[] {
  let start = 0;
  let end = points.length;
  while (start < end && value(points[start]) <= 0) start++;
  while (end > start && value(points[end - 1]) <= 0) end--;
  return start === 0 && end === points.length ? points : points.slice(start, end);
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

export function CardEmpty({ text }: { text: string }) {
  return (
    <div
      style={{
        padding: "var(--sp-7) 0",
        textAlign: "center",
        color: "var(--fg-quaternary)",
        fontSize: "var(--text-sm)",
      }}
    >
      {text}
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
