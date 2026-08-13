import { useId } from "react";
import { CardEmpty } from "./shared";

/* ============================================================================
   Inline SVG charts.

   No chart library and nothing external: every stroke and fill resolves to a
   theme token, so the charts follow light/dark without a second code path.

   The plot is stretched with preserveAspectRatio="none" — cheap and crisp for
   paths (strokes are pinned with vector-effect), but it would distort text, so
   axis labels are rendered as HTML siblings instead.
   ========================================================================== */

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

/** Hit targets carrying the native tooltip — one invisible column per bucket. */
function Hits({ points }: { points: Point[] }) {
  const w = W / Math.max(1, points.length);
  return (
    <>
      {points.map((p, i) => (
        <rect
          key={i}
          className="a-chart__hit"
          x={i * w}
          y={0}
          width={w}
          height={H}
        >
          <title>{p.hint ?? `${p.label} — ${p.value}`}</title>
        </rect>
      ))}
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

function Scale({ max, format }: { max: number; format(n: number): string }) {
  return <div className="a-chart__scale">Peak {format(max)}</div>;
}

/* --- Area chart ---------------------------------------------------------- */

export function AreaChart({
  points,
  height = 180,
  format,
  emptyText = "No data in this period",
}: {
  points: Point[];
  height?: number;
  format(n: number): string;
  emptyText?: string;
}) {
  const gradientId = useId();

  if (!points.length) return <CardEmpty text={emptyText} />;

  const peak = points.reduce((m, p) => Math.max(m, p.value), 0);
  // A flat-zero series still has to draw a baseline rather than divide by zero.
  const max = peak > 0 ? peak * 1.15 : 1;
  const step = points.length > 1 ? W / (points.length - 1) : 0;

  const coords = points.map((p, i) => {
    const x = points.length > 1 ? i * step : W / 2;
    const y = H - (p.value / max) * H;
    return `${x},${isFinite(y) ? y : H}`;
  });

  const line =
    points.length > 1
      ? coords.map((c, i) => `${i ? "L" : "M"}${c}`).join(" ")
      : `M0,${coords[0].split(",")[1]} L${W},${coords[0].split(",")[1]}`;
  const area = `${line} L${W},${H} L0,${H} Z`;

  return (
    <div className="a-chart">
      <Scale max={peak} format={format} />
      <svg
        className="a-chart__plot"
        viewBox={`0 0 ${W} ${H}`}
        preserveAspectRatio="none"
        style={{ height }}
        role="img"
        aria-label={`${points.length} buckets, peak ${format(peak)}`}
      >
        <defs>
          <linearGradient id={gradientId} x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor="var(--accent)" stopOpacity="0.28" />
            <stop offset="100%" stopColor="var(--accent)" stopOpacity="0" />
          </linearGradient>
        </defs>

        <Grid />
        <path d={area} fill={`url(#${gradientId})`} />
        <path
          d={line}
          fill="none"
          stroke="var(--accent)"
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
