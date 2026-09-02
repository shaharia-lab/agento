import { useMemo, useState } from "react";
import { api, qs } from "../lib/api";
import { useResource } from "../lib/hooks";
import { Icon } from "../lib/icons";
import { integer, percent, usd } from "../lib/format";
import { loadAnalyticsPrefs, saveAnalyticsPrefs } from "../lib/analyticsPrefs";
import type {
  AnalyticsReport,
  ClaudeProject,
  InsightsSummary,
} from "../lib/types";
import {
  Checkbox,
  Dropdown,
  Empty,
  InspGroup,
  InspRow,
  Segmented,
  Splitter,
} from "../components/ui";
import {
  RANGE_OPTIONS,
  TZ,
  computePeriod,
  granularityLabel,
  list,
  previousPeriod,
  projectLabel,
  share,
  type Period,
  type RangeKey,
} from "./analytics/shared";
import { TokensMode } from "./analytics/TokensMode";
import { UsageMode } from "./analytics/UsageMode";
import { InsightsMode, insightsInspectorRows } from "./analytics/InsightsMode";
import "../styles/analytics.css";

type Mode = "tokens" | "usage" | "insights";

export function AnalyticsView({
  mode,
  inspectorOpen,
}: {
  mode: Mode;
  inspectorOpen: boolean;
}) {
  const [range, setRange] = useState<RangeKey>("30d");
  const [customFrom, setCustomFrom] = useState("");
  const [customTo, setCustomTo] = useState("");
  const [project, setProject] = useState("");
  const [compare, setCompare] = useState(() => loadAnalyticsPrefs().compare);

  const period = useMemo(
    () => computePeriod(range, customFrom, customTo),
    [range, customFrom, customTo]
  );

  // The window immediately before this one, of the same length. `undefined`
  // means there is nothing well-defined to compare against — All time, or a
  // custom range that is not filled in — and the toggle is shut for it.
  const previousWindow = useMemo(
    () => previousPeriod(range, period),
    [range, period]
  );

  // The two analytics endpoints share a query: range, project and — always —
  // the browser's timezone, which is what the server buckets in.
  const query = qs({
    from: period.from,
    to: period.to,
    project: project || undefined,
    tz: TZ,
  });

  // A half-filled custom range must not fire a request; the server would just
  // fall back to its 31-day default and quietly show the wrong period.
  const ready = range !== "custom" || Boolean(period.from && period.to);
  const wantsReport = mode !== "insights";

  const report = useResource<AnalyticsReport | undefined>(
    (signal) =>
      ready && wantsReport
        ? api.get<AnalyticsReport>(`/claude-analytics${query}`, signal)
        : Promise.resolve(undefined),
    [query, ready, wantsReport]
  );

  // The comparison window, fetched only while the toggle is on and only for the
  // two modes that render one — so an install that never asks for a comparison
  // issues exactly the one request it always did. `qs` drops an undefined
  // `from`/`to`, which the server would answer with its own 31-day default, so
  // the fetch is gated on `previousWindow` rather than on the query string.
  const wantsCompare = compare && wantsReport && ready && previousWindow !== undefined;
  const compareQuery = qs({
    from: previousWindow?.from,
    to: previousWindow?.to,
    project: project || undefined,
    tz: TZ,
  });

  const previous = useResource<AnalyticsReport | undefined>(
    (signal) =>
      wantsCompare
        ? api.get<AnalyticsReport>(`/claude-analytics${compareQuery}`, signal)
        : Promise.resolve(undefined),
    [compareQuery, wantsCompare]
  );

  const insights = useResource<InsightsSummary | undefined>(
    (signal) =>
      ready && mode === "insights"
        ? api.get<InsightsSummary>(
            `/claude-sessions/insights/summary${query}`,
            signal
          )
        : Promise.resolve(undefined),
    [query, ready, mode]
  );

  const projects = useResource<ClaudeProject[] | null>(
    (signal) => api.get<ClaudeProject[] | null>("/claude-sessions/projects", signal),
    []
  );

  const active = mode === "insights" ? insights : report;
  const loading = active.loading;
  const error = active.error;
  const hasData = active.data !== undefined;

  const sessionCount =
    mode === "insights"
      ? insights.data?.total_sessions
      : report.data?.summary.total_sessions;

  // A failed comparison must not take the view down with it: the primary report
  // is what the section is for, so the deltas are dropped and one banner says
  // why. `useResource` keeps the last good `data` on error, so the check is on
  // `error` rather than on `data` being absent — otherwise a stale comparison
  // from a previous window would go on being differenced against this one.
  const comparison = wantsCompare && !previous.error ? previous.data : undefined;

  return (
    <div className="panes">
      <div className="pane-detail">
        <div className="toolbar">
          <Segmented<RangeKey>
            value={range}
            onChange={setRange}
            options={RANGE_OPTIONS}
          />

          {range === "custom" && (
            <>
              <input
                type="date"
                className="a-date"
                value={customFrom}
                max={customTo || undefined}
                onChange={(e) => setCustomFrom(e.target.value)}
                aria-label="From date"
              />
              <span className="toolbar__sub">→</span>
              <input
                type="date"
                className="a-date"
                value={customTo}
                min={customFrom || undefined}
                onChange={(e) => setCustomTo(e.target.value)}
                aria-label="To date"
              />
            </>
          )}

          <ProjectFilter
            value={project}
            onChange={setProject}
            projects={list(projects.data)}
          />

          {wantsReport && (
            <CompareToggle
              on={compare}
              // Disabled rather than silently ignored: "All time" is a floor
              // chosen by the app, not a duration the user picked, so there is
              // no preceding window of the same length to shift onto, and a
              // half-filled custom range has no bounds to shift at all. The
              // stored preference is left exactly as it is — a gate must never
              // rewrite the value it gates (#474) — so switching back to 30d
              // restores the comparison.
              disabled={previousWindow === undefined}
              disabledReason={
                range === "all"
                  ? "All time is not a window of any particular length, so it has nothing before it to compare against."
                  : "Pick both dates to compare this range with the one before it."
              }
              onChange={(v) => {
                setCompare(v);
                saveAnalyticsPrefs({ compare: v });
              }}
            />
          )}

          <div className="spacer" />
          <span className="toolbar__sub tnum">
            {sessionCount === undefined
              ? period.label
              : `${period.label} · ${integer(sessionCount)} sessions`}
          </span>
          <div className="toolbar__sep" />
          <button
            className="iconbtn"
            title="Refresh"
            onClick={() => active.reload()}
          >
            <Icon name="refresh" size={14} />
          </button>
        </div>

        {error && hasData && (
          <div className="banner a-banner--error">
            <Icon name="alert" size={14} />
            <span>Refresh failed — showing the last good data. {error}</span>
          </div>
        )}

        {wantsCompare && previous.error && (
          <div className="banner a-banner--error">
            <Icon name="alert" size={14} />
            <span>
              Comparison unavailable — showing this period on its own.{" "}
              {previous.error}
            </span>
          </div>
        )}

        {!ready ? (
          <div className="a-state">{period.label}</div>
        ) : error && !hasData ? (
          <Empty
            icon="alert"
            title="Could not load analytics"
            text={error}
            action={
              <button className="btn" onClick={() => active.reload()}>
                Try again
              </button>
            }
          />
        ) : !hasData ? (
          <div className="a-state">{loading ? "Loading analytics…" : "No data"}</div>
        ) : (
          <div className={`dash scroll a-dash ${loading ? "a-dash--stale" : ""}`}>
            <Body
              mode={mode}
              report={report.data}
              previous={comparison}
              insights={insights.data}
              project={project}
            />
          </div>
        )}
      </div>

      {inspectorOpen && (
        <>
          <Splitter variable="--inspector-w" min={220} max={420} invert />
          <aside className="pane-inspector">
            <div className="inspector__head">Period</div>
            <div className="inspector__scroll scroll">
              <Inspector
                mode={mode}
                periodLabel={period.label}
                days={period.days}
                from={period.from}
                to={period.to}
                comparedWith={wantsCompare ? previousWindow : undefined}
                project={project}
                report={report.data}
                insights={insights.data}
              />
            </div>
          </aside>
        </>
      )}
    </div>
  );
}

/* --- Body ---------------------------------------------------------------- */

function Body({
  mode,
  report,
  previous,
  insights,
  project,
}: {
  mode: Mode;
  report: AnalyticsReport | undefined;
  /** The preceding window, when the comparison toggle is on and it loaded. */
  previous: AnalyticsReport | undefined;
  insights: InsightsSummary | undefined;
  project: string;
}) {
  const scopeNote = project
    ? `No sessions for ${projectLabel(project)} in this period. Try a wider range or clear the project filter.`
    : "No sessions were recorded in this period. Try a wider range.";

  if (mode === "insights") {
    if (!insights) return null;
    if (insights.total_sessions === 0) {
      return <Empty icon="bulb" title="Nothing to analyse" text={scopeNote} />;
    }
    return <InsightsMode data={insights} />;
  }

  if (!report) return null;
  if (report.summary.total_sessions === 0) {
    return <Empty icon="chart" title="No activity" text={scopeNote} />;
  }

  return mode === "tokens" ? (
    <TokensMode report={report} previous={previous} />
  ) : (
    <UsageMode report={report} previous={previous} />
  );
}

/* --- Compare toggle ------------------------------------------------------ */

/**
 * "Compare with previous period" (#539).
 *
 * The explanation lives on the wrapping `<label>` rather than on the box,
 * because a disabled `<button>` receives no mouse events and a `title` on one
 * never shows — the trap `Switch` already documents. The label is also the
 * second hit target, so the word is clickable when the box is not shut.
 */
function CompareToggle({
  on,
  disabled,
  disabledReason,
  onChange,
}: {
  on: boolean;
  disabled: boolean;
  /** Why the box is shut — there is more than one reason it can be. */
  disabledReason: string;
  onChange(v: boolean): void;
}) {
  return (
    <label
      className="row"
      style={{ gap: "var(--sp-3)", cursor: "default" }}
      title={
        disabled
          ? disabledReason
          : "Show each figure against the window immediately before this one."
      }
    >
      <Checkbox
        on={on}
        disabled={disabled}
        onChange={(v) => {
          if (disabled) return;
          onChange(v);
        }}
      />
      <span className="toolbar__sub">Compare</span>
    </label>
  );
}

/* --- Project filter ------------------------------------------------------ */

/**
 * The server matches on the *decoded* path, not the encoded directory name —
 * filtering by `encoded_name` silently returns zero sessions. For projects the
 * server could not decode the two are identical, so decoded_path is always the
 * right value to send.
 */
function ProjectFilter({
  value,
  onChange,
  projects,
}: {
  value: string;
  onChange(v: string): void;
  projects: ClaudeProject[];
}) {
  const visible = useMemo(
    () =>
      projects
        .filter((p) => !p.hidden && p.session_count > 0)
        .sort((a, b) => b.session_count - a.session_count),
    [projects]
  );

  return (
    <Dropdown
      small
      className="a-select"
      value={value}
      onChange={onChange}
      ariaLabel="Project filter"
      label={value ? projectLabel(value) : "All projects"}
      options={[
        { value: "", label: "All projects" },
        ...visible.map((p) => ({
          value: p.decoded_path,
          label: `${projectLabel(p.decoded_path)} (${p.session_count})`,
        })),
      ]}
    />
  );
}

/* --- Inspector ----------------------------------------------------------- */

function Inspector({
  mode,
  periodLabel,
  days,
  from,
  to,
  comparedWith,
  project,
  report,
  insights,
}: {
  mode: Mode;
  periodLabel: string;
  days: number;
  from?: string;
  to?: string;
  /** The window the figures are being differenced against, if any. */
  comparedWith?: Period;
  project: string;
  report: AnalyticsReport | undefined;
  insights: InsightsSummary | undefined;
}) {
  return (
    <>
      <InspGroup title="Range">
        <InspRow label="Preset">{periodLabel}</InspRow>
        <InspRow label="From">
          <span className="tnum">{isoDay(from)}</span>
        </InspRow>
        <InspRow label="To">
          {/* `to` is the exclusive end of the window, so the last day it covers
              is the day before it. */}
          <span className="tnum">{isoDay(to, -1)}</span>
        </InspRow>
        {days > 0 && (
          <InspRow label="Days">
            <span className="tnum">{days}</span>
          </InspRow>
        )}
        <InspRow label="Timezone">{TZ}</InspRow>
        <InspRow label="Project">
          {project ? projectLabel(project) : "All projects"}
        </InspRow>
        {comparedWith && (
          <>
            <InspRow label="Compared with">{comparedWith.label}</InspRow>
            <InspRow label="From">
              <span className="tnum">{isoDay(comparedWith.from)}</span>
            </InspRow>
            <InspRow label="To">
              <span className="tnum">{isoDay(comparedWith.to, -1)}</span>
            </InspRow>
          </>
        )}
      </InspGroup>

      {mode !== "insights" && report && (
        <>
          <InspGroup title="Totals">
            <InspRow label="Sessions">
              <span className="tnum">{integer(report.summary.total_sessions)}</span>
            </InspRow>
            <InspRow label="Projects">
              <span className="tnum">{integer(report.summary.unique_projects)}</span>
            </InspRow>
            <InspRow label="Active buckets">
              {/* Non-empty only: an all-time range asks for a far wider window
                  than the data, and the charts trim the dead ends off. */}
              <span className="tnum">
                {integer(
                  list(report.time_series).filter((p) => p.total_tokens > 0)
                    .length
                )}{" "}
                {granularityLabel(report.granularity).toLowerCase()}
              </span>
            </InspRow>
            <InspRow label="Most used">{report.summary.most_used_model || "—"}</InspRow>
          </InspGroup>

          <InspGroup title="Cost breakdown">
            <InspRow label="Input">
              <span className="tnum">{usd(report.cost_summary.input_cost_usd)}</span>
            </InspRow>
            <InspRow label="Output">
              <span className="tnum">{usd(report.cost_summary.output_cost_usd)}</span>
            </InspRow>
            <InspRow label="Cache read">
              <span className="tnum">
                {usd(report.cost_summary.cache_read_cost_usd)}
              </span>
            </InspRow>
            <InspRow label="Cache write">
              <span className="tnum">
                {usd(report.cost_summary.cache_write_cost_usd)}
              </span>
            </InspRow>
            <InspRow label="Total">
              <span className="tnum" style={{ color: "var(--green)" }}>
                {usd(report.cost_summary.total_cost_usd)}
              </span>
            </InspRow>
            {report.summary.unknown_pricing_tokens > 0 && (
              <InspRow label="Unpriced">
                <span className="tnum" style={{ color: "var(--amber)" }}>
                  {integer(report.summary.unknown_pricing_tokens)} tok
                </span>
              </InspRow>
            )}
          </InspGroup>

          <InspGroup title="Cache">
            <InspRow label="Hit rate">
              <span className="tnum">{percent(cacheHitRate(report))}</span>
            </InspRow>
            <InspRow label="Read tokens">
              <span className="tnum">
                {integer(report.summary.total_cache_read_tokens)}
              </span>
            </InspRow>
          </InspGroup>
        </>
      )}

      {mode === "insights" && insights && (
        <InspGroup title="Session behaviour">
          {insightsInspectorRows(insights).map((r) => (
            <InspRow key={r.label} label={r.label}>
              <span className="tnum">{r.value}</span>
            </InspRow>
          ))}
        </InspGroup>
      )}
    </>
  );
}

/** Period cache hit rate: cache reads over everything that entered the model. */
function cacheHitRate(report: AnalyticsReport): number {
  const s = report.summary;
  const inputSide =
    s.total_input_tokens + s.total_cache_read_tokens + s.total_cache_creation_tokens;
  return share(s.total_cache_read_tokens, inputSide);
}

/** RFC3339 → `YYYY-MM-DD` in the viewer's zone, optionally shifted by days. */
function isoDay(iso: string | undefined, shiftDays = 0): string {
  if (!iso) return "—";
  const t = new Date(iso).getTime();
  if (!isFinite(t)) return "—";
  const d = new Date(t + shiftDays * 86_400_000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}
