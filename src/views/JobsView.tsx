import {
  Fragment,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { ApiError, api, qs } from "../lib/api";
import type { ClaudeSessionSummary, JobHistory, JobStatus } from "../lib/types";
import { describeError, usePoll, useResource } from "../lib/hooks";
import {
  compactNumber,
  dateTime,
  duration,
  groupByRecency,
  integer,
  relativeTime,
} from "../lib/format";
import { Icon } from "../lib/icons";
import { Empty, InspGroup, InspRow, Search, Segmented, Splitter } from "../components/ui";
import { StatusBadge } from "./TasksView";
import { SessionLink, findSessionById } from "./sessions/SessionLink";
import "../styles/tasks.css";

const PAGE = 50;
const POLL_MS = 5_000;

type Filter = "all" | JobStatus;

const FILTERS: { value: Filter; label: string }[] = [
  { value: "all", label: "All" },
  { value: "running", label: "Running" },
  { value: "success", label: "Success" },
  { value: "failed", label: "Failed" },
];

function totalTokens(j: JobHistory): number {
  return (
    (j.total_input_tokens ?? 0) +
    (j.total_output_tokens ?? 0) +
    (j.total_cache_creation_tokens ?? 0) +
    (j.total_cache_read_tokens ?? 0)
  );
}

/** A running job has no duration_ms yet, so show how long it has been going. */
function runDuration(j: JobHistory): string {
  if (j.duration_ms > 0) return duration(j.duration_ms);
  if (j.status !== "running") return "—";
  const started = new Date(j.started_at).getTime();
  return isFinite(started) ? duration(Date.now() - started) : "—";
}

export function JobsView({
  inspectorOpen,
  openJobId,
  openJobNonce = 0,
}: {
  inspectorOpen: boolean;
  /** A run handed off from a task's *Recent runs*, to select on arrival (#542). */
  openJobId?: string;
  /** `App`'s nav nonce, so the *same* hand-off twice still fires. */
  openJobNonce?: number;
}) {
  // The first page is a live resource so polling only ever re-fetches it;
  // later pages are appended once and left alone.
  const head = useResource<JobHistory[] | null>(
    (signal) => api.get(`/job-history${qs({ limit: PAGE, offset: 0 })}`, signal),
    []
  );

  const [tail, setTail] = useState<JobHistory[]>([]);
  const [exhausted, setExhausted] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [pageError, setPageError] = useState<string>();

  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [focusedId, setFocusedId] = useState<string | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<string>();
  /**
   * The handed-off run, once applied — kept so the "keep the inspector pointed
   * at something that still exists" effect below can tell it apart from a
   * selection that has genuinely gone stale.
   */
  const [openId, setOpenId] = useState<string | null>(null);

  const rows = useMemo(() => {
    // A new run arriving shifts the offset window, so the same record can come
    // back twice across pages.
    const seen = new Set<string>();
    const out: JobHistory[] = [];
    for (const j of [...(head.data ?? []), ...tail]) {
      if (seen.has(j.id)) continue;
      seen.add(j.id);
      out.push(j);
    }
    return out;
  }, [head.data, tail]);

  const anyRunning = rows.some((j) => j.status === "running");
  usePoll(head.reload, POLL_MS, anyRunning);

  const refresh = useCallback(() => {
    setTail([]);
    setExhausted(false);
    setPageError(undefined);
    head.reload();
  }, [head]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return rows.filter((j) => {
      if (filter !== "all" && j.status !== filter) return false;
      if (!q) return true;
      return [j.task_name, j.agent_slug, j.model, j.prompt_preview]
        .join(" ")
        .toLowerCase()
        .includes(q);
    });
  }, [rows, query, filter]);

  const groups = useMemo(
    () => groupByRecency(filtered, (j) => j.started_at),
    [filtered]
  );

  // Keep the inspector pointed at something that still exists.
  useEffect(() => {
    // ...except a handed-off run, which is *legitimately* absent from the
    // loaded page: this list pages 50 at a time and the run a user clicks in a
    // task's Recent runs is usually older than the first page, or excluded by
    // whatever search and filter were left set. Without this guard the arrival
    // is silently replaced by `filtered[0]` — the same steal #536 had to fix in
    // `SessionsView`. `detail` fetches by id, so the inspector renders the run
    // whether or not a row for it exists.
    if (focusedId && focusedId === openId) return;
    if (filtered.length === 0) {
      if (focusedId !== null) setFocusedId(null);
      return;
    }
    if (!focusedId || !filtered.some((j) => j.id === focusedId)) {
      setFocusedId(filtered[0].id);
      setSelected(new Set([filtered[0].id]));
    }
  }, [filtered, focusedId, openId]);

  // The list rows already carry the whole record, but the detail endpoint is
  // the authority — and it picks up a running job's output as it lands.
  const detail = useResource<JobHistory | null>(
    (signal) =>
      focusedId ? api.get(`/job-history/${focusedId}`, signal) : Promise.resolve(null),
    [focusedId]
  );

  const focusedRow = focusedId ? rows.find((j) => j.id === focusedId) ?? null : null;
  // `useResource` keeps the previous `data` when a fetch fails, so the detail is
  // taken only when it is about the run currently focused. Without that check a
  // run whose detail 404s — which is exactly what a hand-off to a deleted run
  // does (#542) — renders the *previously* selected run's output under the new
  // selection, which is worse than reporting nothing.
  const detailRow =
    detail.data && detail.data.id === focusedId ? detail.data : null;
  const job = detailRow ?? focusedRow;

  // A running job's output and timing land after the row was first read.
  usePoll(detail.reload, POLL_MS, job?.status === "running");

  /**
   * A hand-off from a task's *Recent runs* (#542): select that run.
   *
   * Keyed on the **nonce**, never on `rows` — this view is mounted
   * conditionally, so it remounts with an empty page on every arrival and a
   * "is the row already loaded?" fast path would be dead by construction,
   * while re-running on each later page load would yank the user back to a run
   * they had already navigated away from. `App` clears `navTarget` on any
   * navigation carrying none, so a consumed id is not re-applied on a later
   * visit.
   *
   * Nothing is fetched here: `detail` already reads
   * `GET /job-history/{id}` for whatever is focused, and that route answers a
   * real 404 for a run that has been deleted. Paging forward until the run
   * appears was the alternative and is unbounded — it re-reads the whole table
   * for a run from last month.
   */
  const seenOpenNonce = useRef(0);
  /** The handed-off row still waiting to be scrolled to, if any. */
  const scrollTo = useRef<string | null>(null);
  const listRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (openJobNonce === seenOpenNonce.current) return;
    seenOpenNonce.current = openJobNonce;
    if (!openJobId) return;
    setConfirming(false);
    setOpenId(openJobId);
    setFocusedId(openJobId);
    setSelected(new Set([openJobId]));
    // Cleared once the row has actually been scrolled to, or by the user
    // picking a different one — a flag that outlived either would jump the
    // list the next time that row happened to render.
    scrollTo.current = openJobId;
  }, [openJobId, openJobNonce]);

  /**
   * Scroll the handed-off row into view once it exists.
   *
   * `data-job-id` + `querySelector` + `{ block: "nearest" }` is the idiom the
   * shared menus already use (`components/ui.tsx`); a ref per row would have to
   * be keyed on a value that changes with every page.
   */
  useEffect(() => {
    const id = scrollTo.current;
    if (!id) return;
    const row = listRef.current?.querySelector(`[data-job-id="${CSS.escape(id)}"]`);
    if (!row) return;
    scrollTo.current = null;
    row.scrollIntoView({ block: "nearest" });
  }, [groups]);

  async function loadMore() {
    setLoadingMore(true);
    setPageError(undefined);
    try {
      const batch = await api.get<JobHistory[] | null>(
        `/job-history${qs({ limit: PAGE, offset: rows.length })}`
      );
      const list = batch ?? [];
      if (list.length < PAGE) setExhausted(true);
      setTail((t) => [...t, ...list]);
    } catch (err) {
      setPageError(describeError(err));
    } finally {
      setLoadingMore(false);
    }
  }

  function onRowClick(e: React.MouseEvent, j: JobHistory) {
    setConfirming(false);
    // The user has chosen where to look; a hand-off still waiting for its row
    // to appear must not move the list out from under them later.
    scrollTo.current = null;
    if (e.metaKey || e.ctrlKey) {
      setSelected((prev) => {
        const next = new Set(prev);
        if (next.has(j.id)) next.delete(j.id);
        else next.add(j.id);
        return next;
      });
    } else {
      setSelected(new Set([j.id]));
    }
    setFocusedId(j.id);
  }

  async function remove() {
    const ids = [...selected];
    if (ids.length === 0) return;
    setBusy(true);
    setActionError(undefined);
    try {
      if (ids.length === 1) await api.del(`/job-history/${ids[0]}`);
      else await api.del("/job-history", { ids });
      setSelected(new Set());
      setFocusedId(null);
      setConfirming(false);
      refresh();
    } catch (err) {
      setActionError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  const loading = head.loading && !head.data;
  const failedCount = rows.filter((j) => j.status === "failed").length;
  const hasMore = !exhausted && rows.length >= PAGE;

  return (
    <div className="panes">
      <div className="pane-detail">
        <div className="toolbar">
          <div style={{ width: 240 }}>
            <Search value={query} onChange={setQuery} placeholder="Search runs" />
          </div>
          <Segmented<Filter> value={filter} options={FILTERS} onChange={setFilter} />
          <div className="spacer" />

          {confirming ? (
            <div className="confirm">
              <span className="confirm__text">
                Delete {selected.size} {selected.size === 1 ? "run" : "runs"}?
              </span>
              <button className="btn btn--ghost" onClick={() => setConfirming(false)}>
                Cancel
              </button>
              <button className="btn btn--danger" onClick={remove} disabled={busy}>
                Delete
              </button>
            </div>
          ) : (
            <>
              {actionError && <span className="formerror">{actionError}</span>}
              <span className="toolbar__sub tnum">
                {rows.length} {rows.length === 1 ? "run" : "runs"}
                {failedCount > 0 ? ` · ${failedCount} failed` : ""}
                {anyRunning ? " · live" : ""}
              </span>
              <div className="toolbar__sep" />
              <button
                className="iconbtn"
                title={selected.size > 1 ? `Delete ${selected.size} runs` : "Delete run"}
                onClick={() => setConfirming(true)}
                disabled={selected.size === 0}
              >
                <Icon name="trash" size={14} />
              </button>
              <button className="iconbtn" title="Refresh" onClick={refresh}>
                <Icon name="refresh" size={14} />
              </button>
            </>
          )}
        </div>

        {loading ? (
          <div className="statepane">Loading runs…</div>
        ) : head.error && !head.data ? (
          <Empty
            icon="alert"
            title="Couldn't load run history"
            text={head.error}
            action={
              <button className="btn" onClick={refresh}>
                <Icon name="refresh" size={13} />
                Retry
              </button>
            }
          />
        ) : rows.length === 0 ? (
          <Empty
            icon="history"
            title="No runs yet"
            text="Every scheduled task run lands here with its output, timing and token use."
          />
        ) : filtered.length === 0 ? (
          <Empty
            icon="search"
            title="No matching runs"
            text="Nothing matches the current search and filter."
            action={
              <button
                className="btn"
                onClick={() => {
                  setQuery("");
                  setFilter("all");
                }}
              >
                Clear filters
              </button>
            }
          />
        ) : (
          <div className="scroll" style={{ flex: 1, minHeight: 0 }} ref={listRef}>
            <table className="table table--striped">
              <thead>
                <tr>
                  <th style={{ width: "32%" }}>Task</th>
                  <th style={{ width: 160 }}>Agent</th>
                  <th style={{ width: 108 }}>Started</th>
                  <th className="num" style={{ width: 92 }}>
                    Duration
                  </th>
                  <th className="num" style={{ width: 82 }}>
                    Tokens
                  </th>
                  <th style={{ width: 100 }}>Status</th>
                </tr>
              </thead>
              <tbody>
                {groups.map(([group, items]) => (
                  <Fragment key={group}>
                    <tr className="rowgroup">
                      <td colSpan={6}>
                        {group} · {items.length} {items.length === 1 ? "run" : "runs"}
                      </td>
                    </tr>
                    {items.map((j) => (
                      <tr
                        key={j.id}
                        data-job-id={j.id}
                        className={selected.has(j.id) ? "is-selected" : ""}
                        onClick={(e) => onRowClick(e, j)}
                      >
                        <td>{j.task_name || "—"}</td>
                        <td style={{ color: "var(--fg-secondary)" }}>
                          {j.agent_slug || "—"}
                        </td>
                        <td className="tnum" title={dateTime(j.started_at)}>
                          {relativeTime(j.started_at)}
                        </td>
                        <td className="num tnum">{runDuration(j)}</td>
                        <td className="num tnum" title={integer(totalTokens(j))}>
                          {totalTokens(j) ? compactNumber(totalTokens(j)) : "—"}
                        </td>
                        <td>
                          <StatusBadge status={j.status} />
                        </td>
                      </tr>
                    ))}
                  </Fragment>
                ))}
              </tbody>
            </table>

            {(hasMore || pageError) && (
              <div className="loadmore">
                {pageError && <span className="formerror">{pageError}</span>}
                <button className="btn" onClick={loadMore} disabled={loadingMore}>
                  {loadingMore ? "Loading…" : `Load ${PAGE} more`}
                </button>
              </div>
            )}
          </div>
        )}
      </div>

      {inspectorOpen && (
        <>
          <Splitter variable="--inspector-w" min={220} max={420} invert />
          <aside className="pane-inspector">
            <div className="inspector__head">Run</div>
            <div className="inspector__scroll scroll">
              {!job ? (
                // A handed-off run that no longer exists answers a real 404
                // from `GET /job-history/{id}` and has no row to fall back on,
                // so the pane says which of the two it is rather than reading
                // as "nothing selected" or spinning forever.
                <div className="statepane">
                  {focusedId && detail.error
                    ? `Couldn't load this run — ${detail.error}`
                    : focusedId && detail.loading
                    ? "Loading run…"
                    : "Nothing selected"}
                </div>
              ) : (
                <>
                  <InspGroup title="Overview">
                    <InspRow label="Task">{job.task_name || "—"}</InspRow>
                    <InspRow label="Agent">{job.agent_slug || "—"}</InspRow>
                    <InspRow label="Model">{job.model || "—"}</InspRow>
                    <InspRow label="Status">
                      <StatusBadge status={job.status} />
                    </InspRow>
                    <InspRow label="Started">
                      <span title={dateTime(job.started_at)}>
                        {dateTime(job.started_at)}
                      </span>
                    </InspRow>
                    <InspRow label="Finished">
                      {job.finished_at ? dateTime(job.finished_at) : "—"}
                    </InspRow>
                    <InspRow label="Duration">{runDuration(job)}</InspRow>
                  </InspGroup>

                  <InspGroup title="Tokens">
                    <InspRow label="Input">
                      <span className="tnum">{integer(job.total_input_tokens)}</span>
                    </InspRow>
                    <InspRow label="Output">
                      <span className="tnum">{integer(job.total_output_tokens)}</span>
                    </InspRow>
                    <InspRow label="Cache write">
                      <span className="tnum">
                        {integer(job.total_cache_creation_tokens)}
                      </span>
                    </InspRow>
                    <InspRow label="Cache read">
                      <span className="tnum">{integer(job.total_cache_read_tokens)}</span>
                    </InspRow>
                    <InspRow label="Total">
                      <span className="tnum">{integer(totalTokens(job))}</span>
                    </InspRow>
                  </InspGroup>

                  {job.prompt_preview && (
                    <InspGroup title="Prompt">
                      <div className="logblock">{job.prompt_preview}</div>
                    </InspGroup>
                  )}

                  {job.error_message && (
                    <InspGroup title="Error">
                      <div className="logblock logblock--error">
                        {job.error_message}
                      </div>
                    </InspGroup>
                  )}

                  {job.response_text && (
                    <InspGroup title="Output">
                      <div className="logblock">{job.response_text}</div>
                    </InspGroup>
                  )}

                  {!job.response_text && !job.error_message && (
                    <InspGroup title="Output">
                      <div className="runrow">
                        {job.status === "running"
                          ? "Still running…"
                          : "No output was saved for this run."}
                      </div>
                    </InspGroup>
                  )}

                  {job.chat_session_id && (
                    <InspGroup title="Session">
                      <RunSession sessionId={job.chat_session_id} />
                    </InspGroup>
                  )}
                </>
              )}
            </div>
          </aside>
        </>
      )}
    </div>
  );
}

/* --- The run's Claude session --------------------------------------------- */

/**
 * The conversation a run produced, as a control rather than a string (#542).
 *
 * `job_history` stores the session **id** and nothing else, and an id is not a
 * promise that the session is there to open: the scan may not have reached the
 * transcript yet, or its project may be hidden. Handing the id straight to
 * `SessionLink` would offer a button that lands the user in the Sessions
 * section with nothing to show, so the row is resolved first and the control is
 * only offered once it is known to exist — `sessionMenuItems`' own rule that
 * "unknown" is not "absent", one level up.
 *
 * The lookup is `findSessionById` — the cheap list read scoped to the id —
 * with `GET /claude-sessions/{id}` as the fallback **on a miss only**, which is
 * `SessionsView`'s own hand-off resolution and is here for the same reason.
 * The list reads `claude_session_cache`, and a scan is only forced once the
 * cache is an hour old (`native/scan.rs`'s `CACHE_TTL`), so the session a run
 * *just* produced is normally not in it — while the by-id route re-reads the
 * transcript off disk and answers for a session the scanner has never reached.
 * Concluding "absent" from the list alone would therefore withhold the control
 * for exactly the freshest runs, which are the ones somebody clicks in *Recent
 * runs*. The order is what keeps that affordable: the by-id read answers the
 * summary *plus every message*, so it is never paid for a session the list can
 * already see, and only a **404** from it is a real absence.
 */
function RunSession({ sessionId }: { sessionId: string }) {
  type Resolved =
    | { kind: "pending" }
    | { kind: "found"; row: ClaudeSessionSummary }
    | { kind: "absent" }
    | { kind: "failed"; message: string };

  const [resolved, setResolved] = useState<Resolved>({ kind: "pending" });

  useEffect(() => {
    // Aborted as well as flagged: `StrictMode` runs this twice in development.
    const ctl = new AbortController();
    let cancelled = false;
    setResolved({ kind: "pending" });
    findSessionById(sessionId, ctl.signal)
      .then(
        (hit) =>
          hit ??
          // Typed as the summary because that is all this needs; the route
          // answers a `ClaudeSessionDetail`, which is that plus the transcript.
          api.get<ClaudeSessionSummary>(
            `/claude-sessions/${sessionId}`,
            ctl.signal
          )
      )
      .then((row) => {
        if (cancelled) return;
        setResolved({ kind: "found", row });
      })
      .catch((err) => {
        if (cancelled) return;
        // Only the by-id route's 404 says the session is not there. Anything
        // else is a failed *lookup*, which is not evidence the session is gone
        // — reporting a transient error as an absence states something untrue
        // about the user's data.
        setResolved(
          err instanceof ApiError && err.status === 404
            ? { kind: "absent" }
            : { kind: "failed", message: describeError(err) }
        );
      });
    return () => {
      cancelled = true;
      ctl.abort();
    };
  }, [sessionId]);

  if (resolved.kind === "found") {
    return (
      // No `title`: the id is what this pane has always shown, and losing it
      // would take the one string somebody debugging a run copies out of here.
      // `project_path` comes from the resolved row — never anything merely
      // path-shaped, per `SessionLink`'s own note on that prop.
      <SessionLink
        sessionId={sessionId}
        projectPath={resolved.row.project_path || undefined}
      />
    );
  }

  return (
    <>
      <div className="logblock">{sessionId}</div>
      <div className="runrow">
        {resolved.kind === "pending"
          ? "Looking for this session…"
          : resolved.kind === "failed"
          ? `Couldn't look this session up — ${resolved.message}`
          : "This run's session is no longer on disk, so there is nothing to open."}
      </div>
    </>
  );
}
