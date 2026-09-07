import { useId } from "react";
import "../styles/charts.css";

/* ============================================================================
   Inline SVG charts.

   No chart library and nothing external: every stroke and fill resolves to a
   theme token, so the charts follow light/dark without a second code path.

   The plot is stretched with preserveAspectRatio="none" — cheap and crisp for
   paths (strokes are pinned with vector-effect), but it would distort text, so
   axis labels are rendered as HTML siblings instead.

   This module lives under `components/` rather than under `views/analytics/`
   (#428) because it is pure presentation over `{label, value, hint}[]` and has
   no Claude semantics in it: the LLM Gateway's Usage dashboard draws the same
   charts over an entirely separate table. The locked decision that the gateway
   shares nothing with Claude analytics is about *data* and *sections*; a second
   copy of 250 lines of SVG would only guarantee the two drift.

   It imports nothing from `views/`, and its stylesheet moved with it: every
   `.a-chart*` / `.a-heat*` rule came out of `styles/analytics.css` verbatim, so
   the class names — and therefore every existing chart's rendering — are
   unchanged. `CardEmpty` moved here from `views/analytics/shared.tsx` for the
   same reason and is re-exported from there, so no analytics call site churns.
   ========================================================================== */

/** The empty state every chart falls back to, and the only thing they share. */
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

export interface Point {
  label: string;
  value: number;
  /** Tooltip line; falls back to `label — value`. */
  hint?: string;
}

const W = 100;
const H = 100;
const GRID = [0, 25, 50, 75, 100];

function Grid() {
  return (
    <>
      {GRID.map((y) => (
        <line
          key={y}
          x1="0"
          y1={y}
          x2={W}
          y2={y}
          stroke="var(--line-soft)"
          strokeWidth="0.4"
          vectorEffect="non-scaling-stroke"
        />
      ))}
    </>
  );
}

/**
 * Hit targets carrying the native tooltip — one invisible column per bucket.
 *
 * There is one column per *current* bucket whether or not a comparison series
 * is drawn: the comparison is an overlay on this axis, not a second axis, so a
 * second row of hit targets would put two tooltips over one column. When
 * `compare` is given, its aligned value is appended to the same `<title>`.
 */
function Hits({
  points,
  compare,
  offset = 0,
  compareText,
}: {
  points: Point[];
  compare?: Point[];
  /** Index in `points` the aligned comparison series starts at. */
  offset?: number;
  compareText?: (p: Point) => string;
}) {
  const w = W / Math.max(1, points.length);
  return (
    <>
      {points.map((p, i) => {
        const base = p.hint ?? `${p.label} — ${p.value}`;
        const other = compare?.[i - offset];
        return (
          <rect
            key={i}
            className="a-chart__hit"
            x={i * w}
            y={0}
            width={w}
            height={H}
          >
            <title>
              {other && compareText ? `${base} · ${compareText(other)}` : base}
            </title>
          </rect>
        );
      })}
    </>
  );
}

/** First / middle / last, so the axis never collides with itself. */
function Axis({ points }: { points: Point[] }) {
  if (points.length < 2) {
    return (
      <div className="a-chart__axis">
        <span>{points[0]?.label ?? ""}</span>
      </div>
    );
  }
  const mid = points[Math.floor(points.length / 2)];
  return (
    <div className="a-chart__axis">
      <span>{points[0].label}</span>
      {points.length > 4 && <span>{mid.label}</span>}
      <span>{points[points.length - 1].label}</span>
    </div>
  );
}

function Scale({
  max,
  format,
  note,
}: {
  max: number;
  format(n: number): string;
  note?: string;
}) {
  return (
    <div className="a-chart__scale">
      Peak {format(max)}
      {note && ` · ${note}`}
    </div>
  );
}


/* --- Area chart ---------------------------------------------------------- */

export function AreaChart({
  points,
  compare,
  compareLabel = "previous period",
  height = 180,
  format,
  emptyText = "No data in this period",
}: {
  points: Point[];
  /**
   * An optional second series drawn as a stroke-only dashed line over the same
   * axes (#539) — typically the same metric over the window immediately before
   * this one.
   *
   * **It shares one y-maximum with `points`, and that is the load-bearing part**:
   * two independently scaled lines would put a smaller series above a larger one,
   * which is a false crossover rather than a comparison.
   *
   * **Alignment is from the last element backwards**, truncated to the shorter
   * of the two. Two windows of equal *duration* can still produce different
   * bucket counts — a weekly walk starting on a different weekday, unequal month
   * lengths, an hourly walk across a DST transition — so zipping from index 0
   * would slide the whole overlay by a bucket, and joining on the date key
   * cannot work at all when the two windows share no dates by construction.
   */
  compare?: Point[];
  /** How the comparison series is named in the caption and the tooltips. */
  compareLabel?: string;
  height?: number;
  format(n: number): string;
  emptyText?: string;
}) {
  const gradientId = useId();

  if (!points.length) return <CardEmpty text={emptyText} />;

  // Aligned from the last element backwards; a comparison longer than the
  // current series loses its *oldest* buckets, which are the ones with no
  // counterpart on this axis.
  const cmp = compare?.length
    ? compare.slice(Math.max(0, compare.length - points.length))
    : undefined;
  const offset = points.length - (cmp?.length ?? 0);

  // Two maxima, and they are for two different things. `peak` is the *current*
  // window's own, and it is the only one named anywhere: it is exact, because
  // the caller's trim removes zero buckets only. `scaleTo` also bounds the
  // comparison, because the two lines have to share a y-maximum or a smaller
  // series draws above a larger one, which is a false crossover rather than a
  // comparison — but it is deliberately **not** captioned. `cmp` is a truncated,
  // window-aligned slice of the comparison, so its maximum describes the drawn
  // span rather than the period, and a number captioned "previous period peak"
  // would attribute a figure to a window that may never have had it.
  const peak = points.reduce((m, p) => Math.max(m, p.value), 0);
  const scaleTo = Math.max(
    peak,
    cmp?.reduce((m, p) => Math.max(m, p.value), 0) ?? 0
  );
  // A flat-zero series still has to draw a baseline rather than divide by zero.
  const max = scaleTo > 0 ? scaleTo * 1.15 : 1;
  const step = points.length > 1 ? W / (points.length - 1) : 0;
  const y = (v: number) => {
    const at = H - (v / max) * H;
    return isFinite(at) ? at : H;
  };

  const coords = points.map((p, i) => {
    const x = points.length > 1 ? i * step : W / 2;
    return `${x},${y(p.value)}`;
  });

  const line =
    points.length > 1
      ? coords.map((c, i) => `${i ? "L" : "M"}${c}`).join(" ")
      : `M0,${coords[0].split(",")[1]} L${W},${coords[0].split(",")[1]}`;
  const area = `${line} L${W},${H} L0,${H} Z`;

  // A single comparison bucket against a multi-bucket axis has no segment to
  // draw — one point is not a line. Its value still reaches the tooltip.
  const compareLine =
    cmp && cmp.length > 1
      ? cmp.map((p, j) => `${j ? "L" : "M"}${(offset + j) * step},${y(p.value)}`).join(" ")
      : cmp && cmp.length === 1 && points.length === 1
        ? `M0,${y(cmp[0].value)} L${W},${y(cmp[0].value)}`
        : undefined;

  return (
    <div className="a-chart">
      <Scale
        max={peak}
        format={format}
        note={cmp ? `dashed = ${compareLabel}` : undefined}
      />
      <svg
        className="a-chart__plot"
        viewBox={`0 0 ${W} ${H}`}
        preserveAspectRatio="none"
        style={{ height }}
        role="img"
        aria-label={
          cmp
            ? `${points.length} buckets, peak ${format(peak)}, with the ${compareLabel} overlaid`
            : `${points.length} buckets, peak ${format(peak)}`
        }
      >
        <defs>
          <linearGradient id={gradientId} x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor="var(--accent)" stopOpacity="0.28" />
            <stop offset="100%" stopColor="var(--accent)" stopOpacity="0" />
          </linearGradient>
        </defs>

        <Grid />
        <path d={area} fill={`url(#${gradientId})`} />
        {compareLine && (
          <path
            className="a-chart__compare"
            d={compareLine}
            vectorEffect="non-scaling-stroke"
          />
        )}
        <path
          d={line}
          fill="none"
          stroke="var(--accent)"
          strokeWidth="1.6"
          strokeLinejoin="round"
          strokeLinecap="round"
          vectorEffect="non-scaling-stroke"
        />
        <Hits
          points={points}
          compare={cmp}
          offset={offset}
          compareText={(p) => `${compareLabel}: ${format(p.value)}`}
        />
      </svg>
      <Axis points={points} />
    </div>
  );
}

/* --- Bar chart ----------------------------------------------------------- */

export function BarChart({
  points,
  height = 160,
  format,
  emptyText = "No data in this period",
  tone = "var(--accent)",
}: {
  points: Point[];
  height?: number;
  format(n: number): string;
  emptyText?: string;
  tone?: string;
}) {
  if (!points.length) return <CardEmpty text={emptyText} />;

  const peak = points.reduce((m, p) => Math.max(m, p.value), 0);
  const max = peak > 0 ? peak : 1;
  const slot = W / points.length;
  const barW = Math.max(slot * 0.62, slot - 1.2);

  return (
    <div className="a-chart">
      <Scale max={peak} format={format} />
      <svg
        className="a-chart__plot"
        viewBox={`0 0 ${W} ${H}`}
        preserveAspectRatio="none"
        style={{ height }}
        role="img"
        aria-label={`${points.length} bars, peak ${format(peak)}`}
      >
        <Grid />
        {points.map((p, i) => {
          const h = p.value > 0 ? (p.value / max) * H : 0;
          return (
            <rect
              key={i}
              x={i * slot + (slot - barW) / 2}
              y={H - h}
              width={barW}
              height={h}
              fill={tone}
              rx="0.6"
            />
          );
        })}
        <Hits points={points} />
      </svg>
      <Axis points={points} />
    </div>
  );
}

/* --- Line chart on a fixed 0-100 domain (rates) -------------------------- */

export function RateChart({
  points,
  height = 180,
  emptyText = "No data in this period",
}: {
  points: Point[];
  height?: number;
  emptyText?: string;
}) {
  if (!points.length) return <CardEmpty text={emptyText} />;

  // A hit rate is only readable against a fixed 0-100 axis; autoscaling it
  // would make a 3-point wobble look like a collapse.
  const step = points.length > 1 ? W / (points.length - 1) : 0;
  const y = (v: number) => {
    const clamped = Math.min(100, Math.max(0, isFinite(v) ? v : 0));
    return H - clamped;
  };

  const line =
    points.length > 1
      ? points
          .map((p, i) => `${i ? "L" : "M"}${i * step},${y(p.value)}`)
          .join(" ")
      : `M0,${y(points[0].value)} L${W},${y(points[0].value)}`;

  return (
    <div className="a-chart">
      <div className="a-chart__scale">Scale 0–100%</div>
      <svg
        className="a-chart__plot"
        viewBox={`0 0 ${W} ${H}`}
        preserveAspectRatio="none"
        style={{ height }}
        role="img"
        aria-label={`Rate over ${points.length} buckets`}
      >
        <Grid />
        <path
          d={line}
          fill="none"
          stroke="var(--green)"
          strokeWidth="1.6"
          strokeLinejoin="round"
          strokeLinecap="round"
          vectorEffect="non-scaling-stroke"
        />
        <Hits points={points} />
      </svg>
      <Axis points={points} />
    </div>
  );
}

/* --- Day-of-week × hour heatmap ------------------------------------------ */

const DOW = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const HOURS = Array.from({ length: 24 }, (_, h) => h);

export function Heatmap({
  cells,
  emptyText = "No activity in this period",
}: {
  cells: { day_of_week: number; hour: number; sessions: number }[];
  emptyText?: string;
}) {
  if (!cells.length) return <CardEmpty text={emptyText} />;

  // 7×24 grid keyed by dow*24+hour; the server only sends non-empty cells.
  const grid = new Array<number>(7 * 24).fill(0);
  let peak = 0;
  for (const c of cells) {
    if (c.day_of_week < 0 || c.day_of_week > 6) continue;
    if (c.hour < 0 || c.hour > 23) continue;
    const i = c.day_of_week * 24 + c.hour;
    grid[i] += c.sessions;
    peak = Math.max(peak, grid[i]);
  }
  if (peak <= 0) return <CardEmpty text={emptyText} />;

  return (
    <div>
      <div className="a-heat">
        {DOW.map((name, d) => (
          <Row key={name} name={name} day={d} grid={grid} peak={peak} />
        ))}
      </div>
      <div className="a-heat__hours">
        <span />
        {HOURS.map((h) => (
          <span className="a-heat__hour" key={h}>
            {h % 3 === 0 ? h : ""}
          </span>
        ))}
      </div>
    </div>
  );
}

function Row({
  name,
  day,
  grid,
  peak,
}: {
  name: string;
  day: number;
  grid: number[];
  peak: number;
}) {
  return (
    <>
      <div className="a-heat__label">{name}</div>
      {HOURS.map((h) => {
        const v = grid[day * 24 + h];
        return (
          <div
            key={h}
            className={`a-heat__cell ${v > 0 ? "a-heat__cell--on" : ""}`}
            // 0.12 floor keeps a single session visible against the track.
            style={v > 0 ? { opacity: 0.12 + 0.88 * (v / peak) } : undefined}
            title={`${name} ${String(h).padStart(2, "0")}:00 — ${v} session${
              v === 1 ? "" : "s"
            }`}
          />
        );
      })}
    </>
  );
}
