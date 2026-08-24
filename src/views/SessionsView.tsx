import {
  Fragment,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { api, qs } from "../lib/api";
import type {
  ClaudeProject,
  ClaudeSessionSummary,
  SessionCost,
  SessionFacets,
  SessionPage,
  SessionScanStatus,
  TokenUsage,
} from "../lib/types";
import { describeError, useDebounced, usePoll, useResource } from "../lib/hooks";
import {
  compactNumber,
  dateTime,
  duration,
  groupByRecency,
  integer,
  relativeTime,
  tildePath,
  usd,
} from "../lib/format";
import { Icon } from "../lib/icons";
import { snippetParts, snippetText } from "../lib/snippet";
import { sessionAgentName } from "../lib/sessionAgent";
import { openExternal } from "../lib/tauri";
import {
  Dropdown,
  Empty,
  InspGroup,
  InspRow,
  Search,
  Splitter,
} from "../components/ui";
import { SessionDetail } from "./sessions/SessionDetail";
import "../styles/sessions.css";

/**
 * A full-width list view — column headers, tabular numerals, alternating rows,
 * grouped section headers. This is the native table idiom, not a card grid.
 *
 * Every number a row shows folds the session's sub-agent work into the parent,
 * because the backend's own aggregates do: a row that counted only the main
 * thread would not add up to the totals in the toolbar.
 */

const PAGE_SIZE = 50;

type Sort = "recent" | "cost" | "tokens" | "duration" | "messages" | "relevance";

const SORTS: { value: Sort; label: string }[] = [
  { value: "relevance", label: "Relevance" },
  { value: "recent", label: "Recent" },
  { value: "cost", label: "Cost" },
  { value: "tokens", label: "Tokens" },
  { value: "duration", label: "Duration" },
  { value: "messages", label: "Messages" },
];

interface Filters {
  project: string;
  /**
   * The sort the user picked, or `""` while it follows the server's default.
   *
   * The empty value is what makes "reflect the server default" possible without
   * a second source of truth: the effective sort is [`resolveSort`], a mirror of
   * `sessions/query.rs::resolve_sort`, so the dropdown's label always states
   * what the server will actually do.
   */
  sort: Sort | "";
  favorites: boolean;
}

const INITIAL_FILTERS: Filters = {
  project: "",
  sort: "",
  favorites: false,
};

/**
 * The sort a request will really run under — `sessions/query.rs::resolve_sort`,
 * mirrored so the control cannot claim an ordering the server would not use.
 *
 * Two rules, both the server's:
 *
 * * **No explicit choice plus a search term is `relevance`.** Somebody who typed
 *   a query wants the best match first; somebody who did not has no ranking to
 *   sort by. An explicit pick always wins, so a user who chose "Recent" keeps it
 *   while typing.
 * * **`relevance` with no search term is `recent`**, because without a `MATCH`
 *   there is no rank. That is also what restores the pre-search ordering when
 *   the query is cleared: the relevance the search selected simply stops
 *   applying, and an explicit pick made before the search is still in `filters`.
 */
function resolveSort(chosen: Sort | "", searching: boolean): Sort {
  if (!chosen) return searching ? "relevance" : "recent";
  if (chosen === "relevance" && !searching) return "recent";
  return chosen;
}

/** Rows accumulated across pages, tagged with the filter set that produced them. */
interface Loaded {
  key: string;
  items: ClaudeSessionSummary[];
  nextCursor: string;
  hasMore: boolean;
}

const NO_ROWS: ClaudeSessionSummary[] = [];

/* --- Totals -------------------------------------------------------------- */

const ZERO_USAGE: TokenUsage = {
  input_tokens: 0,
  output_tokens: 0,
  cache_creation_tokens: 0,
  cache_creation_5m_tokens: 0,
  cache_creation_1h_tokens: 0,
  cache_read_tokens: 0,
};

const ZERO_COST: SessionCost = {
  input_usd: 0,
  output_usd: 0,
  cache_read_usd: 0,
  cache_write_usd: 0,
  total_usd: 0,
};

function usageOf(u: TokenUsage | undefined | null): TokenUsage {
  return u ?? ZERO_USAGE;
}

function costOf(c: SessionCost | undefined | null): SessionCost {
  return c ?? ZERO_COST;
}

/**
 * Billable input/output, main thread plus sub-agents. Cache tokens are billed
 * separately and are deliberately not folded in here — `facets.total_tokens`
 * is exactly the sum of these two numbers over the filtered set.
 */
function tokensIn(s: ClaudeSessionSummary): number {
  return usageOf(s.usage).input_tokens + usageOf(s.subagent_usage).input_tokens;
}

function tokensOut(s: ClaudeSessionSummary): number {
  return (
    usageOf(s.usage).output_tokens + usageOf(s.subagent_usage).output_tokens
  );
}

function totalCost(s: ClaudeSessionSummary): number {
  return costOf(s.cost).total_usd + costOf(s.subagent_cost).total_usd;
}

function totalDuration(s: ClaudeSessionSummary): number {
  return (s.active_duration_ms ?? 0) + (s.subagent_active_duration_ms ?? 0);
}

/* --- Presentation helpers ------------------------------------------------- */

function modeBadge(s: ClaudeSessionSummary): { label: string; tone: string } | null {
  switch (s.permission_mode) {
    case "bypassPermissions":
      return { label: "Bypass", tone: "badge--amber" };
    case "plan":
      return { label: "Plan", tone: "badge--purple" };
    case "acceptEdits":
      return { label: "Accept", tone: "badge--teal" };
    case "dontAsk":
      return { label: "Don't ask", tone: "badge--teal" };
    case "default":
      return { label: "Default", tone: "" };
    default:
      return s.mode ? { label: s.mode, tone: "" } : null;
  }
}

/**
 * The second line under a row's title: why this row matched.
 *
 * Rendered only for a **content** match — a row that matched on its id, its
 * project path or its title carries no snippet, and gets no extra line, which
 * is what keeps an ordinary listing at its 28px row height.
 *
 * **The markers are the only markup honoured.** Every segment goes through JSX
 * as a text child, so a transcript containing `<script>` renders as the literal
 * characters; nothing here builds an HTML string and nothing may reach for
 * `dangerouslySetInnerHTML`. `snippetParts` also strips any stray sentinel, so
 * a malformed snippet cannot leak a control character into the cell.
 */
function MatchSnippet({ snippet }: { snippet?: string }) {
  const raw = snippet ?? "";
  const parts = useMemo(() => snippetParts(raw), [raw]);
  if (parts.length === 0) return null;
  return (
    <span className="sess-snippet" title={snippetText(raw)}>
      {parts.map((p, i) =>
        p.hit ? (
          <mark className="sess-snippet__hit" key={i}>
            {p.text}
          </mark>
        ) : (
          <Fragment key={i}>{p.text}</Fragment>
        )
      )}
    </span>
  );
}

/**
 * A scan that has never run reports a zero timestamp, which must read as
 * "not indexed yet" rather than as an empty index.
 */
function neverScanned(st: SessionScanStatus | undefined): boolean {
  if (!st) return false;
  if (!st.last_scanned_at) return true;
  const t = new Date(st.last_scanned_at).getTime();
  return !isFinite(t) || new Date(t).getFullYear() < 2000;
}

export function SessionsView({ inspectorOpen }: { inspectorOpen: boolean }) {
  const [query, setQuery] = useState("");
  const q = useDebounced(query, 250);
  const [filters, setFilters] = useState<Filters>(INITIAL_FILTERS);

  // The debounced term, not the raw one: gating relevance on what the user is
  // still typing would flicker the option in and out per keystroke and refetch
  // the list ahead of the debounce it exists to respect.
  const searching = q.trim() !== "";
  const sort = resolveSort(filters.sort, searching);

  // Every filter and the *resolved* sort belong to the key: the keyset cursor
  // encodes the sort, so re-using one across a change is a 400 from the server —
  // and it is the resolved value the request carries.
  const listKey = useMemo(
    () => JSON.stringify({ q, ...filters, sort }),
    [q, filters, sort]
  );

  const [paging, setPaging] = useState({ key: "", cursor: "" });
  const cursor = paging.key === listKey ? paging.cursor : "";

  const [loaded, setLoaded] = useState<Loaded>({
    key: "",
    items: [],
    nextCursor: "",
    hasMore: false,
  });
  const items = loaded.key === listKey ? loaded.items : NO_ROWS;
  const hasMore = loaded.key === listKey && loaded.hasMore;

  const filterParams = useMemo(
    () => ({
      q: q.trim() || undefined,
      project: filters.project || undefined,
      favorites: filters.favorites ? true : undefined,
    }),
    [q, filters.project, filters.favorites]
  );

  const page = useResource<SessionPage>(
    (signal) =>
      api.get<SessionPage>(
        `/claude-sessions${qs({
          ...filterParams,
          sort,
          limit: PAGE_SIZE,
          cursor: cursor || undefined,
        })}`,
        signal
      ),
    [listKey, cursor]
  );

  const facets = useResource<SessionFacets>(
    (signal) =>
      api.get<SessionFacets>(`/claude-sessions/facets${qs(filterParams)}`, signal),
    [listKey]
  );

  const projects = useResource<ClaudeProject[]>(
    (signal) => api.get<ClaudeProject[]>("/claude-sessions/projects", signal),
    []
  );

  const status = useResource<SessionScanStatus>(
    (signal) => api.get<SessionScanStatus>("/claude-sessions/status", signal),
    []
  );

  /* --- Page accumulation -------------------------------------------------- */

  // Applying by response identity keeps a re-run of this effect (StrictMode, or
  // a cursor change landing before the next response) from appending twice.
  const appliedRef = useRef<SessionPage | null>(null);

  useEffect(() => {
    const data = page.data;
    if (!data || appliedRef.current === data) return;
    appliedRef.current = data;

    const incoming = data.items ?? [];
    setLoaded((prev) => {
      const base = prev.key === listKey && cursor !== "" ? prev.items : [];
      const seen = new Set(base.map((s) => s.session_id));
      return {
        key: listKey,
        items: base.concat(incoming.filter((s) => !seen.has(s.session_id))),
        nextCursor: data.next_cursor ?? "",
        hasMore: Boolean(data.has_more),
      };
    });
  }, [page.data, listKey, cursor]);

  const patchFilters = useCallback((patch: Partial<Filters>) => {
    // Batched with the filter change so no request ever carries a stale cursor.
    setPaging({ key: "", cursor: "" });
    setFilters((f) => ({ ...f, ...patch }));
  }, []);

  const onQuery = useCallback((v: string) => {
    setPaging({ key: "", cursor: "" });
    setQuery(v);
  }, []);

  const loadMore = useCallback(() => {
    if (!loaded.nextCursor) return;
    setPaging({ key: listKey, cursor: loaded.nextCursor });
  }, [listKey, loaded.nextCursor]);

  const reloadList = page.reload;
  const reloadFacets = facets.reload;
  const reloadProjects = projects.reload;

  const reloadAll = useCallback(() => {
    setPaging({ key: "", cursor: "" });
    reloadList();
    reloadFacets();
    reloadProjects();
  }, [reloadList, reloadFacets, reloadProjects]);

  /* --- Rescan ------------------------------------------------------------- */

  const [refreshPending, setRefreshPending] = useState(false);
  const [refreshError, setRefreshError] = useState<string>();
  const scanMark = useRef("");
  const wasScanning = useRef(false);

  const scanning = refreshPending || (status.data?.scan_in_progress ?? false);
  usePoll(status.reload, 1000, scanning);

  useEffect(() => {
    const st = status.data;
    if (!st) return;
    if (st.scan_in_progress) {
      wasScanning.current = true;
      setRefreshPending(false);
      return;
    }
    // A scan that started and finished between two polls still bumps the
    // timestamp, so that is what tells us our own request completed.
    const finished =
      wasScanning.current ||
      (refreshPending && st.last_scanned_at !== scanMark.current);
    if (!finished) return;
    wasScanning.current = false;
    setRefreshPending(false);
    reloadAll();
  }, [status.data, refreshPending, reloadAll]);

  const refresh = useCallback(async () => {
    setRefreshError(undefined);
    scanMark.current = status.data?.last_scanned_at ?? "";
    setRefreshPending(true);
    try {
      await api.post<void>("/claude-sessions/refresh");
      status.reload();
    } catch (err) {
      setRefreshPending(false);
      setRefreshError(describeError(err));
    }
  }, [status]);

  /* --- Selection ---------------------------------------------------------- */

  const [selectedId, setSelectedId] = useState<string>();
  const [lastSelected, setLastSelected] = useState<ClaudeSessionSummary>();
  /** Session whose transcript fills the pane; unset shows the table. */
  const [openId, setOpenId] = useState<string>();

  // Prefer the row in the current page set so favourite toggles show through;
  // fall back to the last selection while a reload has emptied the table.
  const selected = useMemo(() => {
    const inView = items.find((s) => s.session_id === selectedId);
    return inView ?? (lastSelected?.session_id === selectedId ? lastSelected : undefined);
  }, [items, selectedId, lastSelected]);

  const select = useCallback((s: ClaudeSessionSummary) => {
    setSelectedId(s.session_id);
    setLastSelected(s);
  }, []);

  const openSession = useMemo(() => {
    if (!openId) return undefined;
    return (
      items.find((s) => s.session_id === openId) ??
      (lastSelected?.session_id === openId ? lastSelected : undefined)
    );
  }, [openId, items, lastSelected]);

  // Single click selects, and the transcript opens on an explicit action:
  // double-click, the inspector's "View session", or Enter. Rows never take
  // focus, so Enter is only claimed while nothing focusable holds it — a
  // focused button or the search field keeps its own Enter.
  useEffect(() => {
    if (openId) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Enter" || e.metaKey || e.ctrlKey || e.altKey || e.shiftKey)
        return;
      const active = document.activeElement;
      if (active && active !== document.body) return;
      if (!selectedId) return;
      e.preventDefault();
      setOpenId(selectedId);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [openId, selectedId]);

  // A native list always has a selection: take the first row whenever the
  // current one is not among the loaded rows.
  useEffect(() => {
    if (!items.length) return;
    if (items.some((s) => s.session_id === selectedId)) return;
    select(items[0]);
  }, [items, selectedId, select]);

  /* --- Row actions -------------------------------------------------------- */

  const [busy, setBusy] = useState<"favorite" | "continue">();
  const [actionError, setActionError] = useState<string>();
  const [continuedChat, setContinuedChat] = useState<string>();

  const applyPatch = useCallback(
    (id: string, patch: Partial<ClaudeSessionSummary>) => {
      setLoaded((prev) => ({
        ...prev,
        items: prev.items.map((s) =>
          s.session_id === id ? { ...s, ...patch } : s
        ),
      }));
      setLastSelected((prev) =>
        prev && prev.session_id === id ? { ...prev, ...patch } : prev
      );
    },
    []
  );

  const toggleFavorite = useCallback(
    async (s: ClaudeSessionSummary) => {
      const next = !s.is_favorite;
      setBusy("favorite");
      setActionError(undefined);
      try {
        await api.patch<void>(`/claude-sessions/${s.session_id}`, {
          is_favorite: next,
        });
        applyPatch(s.session_id, { is_favorite: next });
        reloadFacets();
      } catch (err) {
        setActionError(describeError(err));
      } finally {
        setBusy(undefined);
      }
    },
    [applyPatch, reloadFacets]
  );

  const continueInChat = useCallback(async (s: ClaudeSessionSummary) => {
    setBusy("continue");
    setActionError(undefined);
    setContinuedChat(undefined);
    try {
      const res = await api.post<{ chat_id: string }>(
        `/claude-sessions/${s.session_id}/continue`
      );
      setContinuedChat(res?.chat_id ?? "");
    } catch (err) {
      setActionError(describeError(err));
    } finally {
      setBusy(undefined);
    }
  }, []);

  useEffect(() => {
    setActionError(undefined);
    setContinuedChat(undefined);
  }, [selectedId]);

  /* --- Derived view data --------------------------------------------------- */

  const groups = useMemo(
    () => groupByRecency(items, (s) => s.last_activity),
    [items]
  );

  // The p90 of the filtered set is what the meter is scaled against, so one
  // huge session cannot flatten every other bar.
  const meterScale = useMemo(() => {
    const p90 = facets.data?.token_p90 ?? 0;
    if (p90 > 0) return p90;
    return items.reduce((max, s) => Math.max(max, tokensIn(s) + tokensOut(s)), 1);
  }, [facets.data, items]);

  const projectOptions = useMemo(() => {
    const list = (projects.data ?? []).filter((p) => !p.hidden);
    return [...list].sort(
      (a, b) =>
        b.session_count - a.session_count ||
        a.decoded_path.localeCompare(b.decoded_path)
    );
  }, [projects.data]);

  const filtersActive = Boolean(
    query.trim() || filters.project || filters.favorites
  );
  const loadingMore = page.loading && cursor !== "";
  const remaining = facets.data ? facets.data.total - items.length : 0;

  const summary = facets.data
    ? `${integer(facets.data.total)} ${
        facets.data.total === 1 ? "session" : "sessions"
      } · ${compactNumber(facets.data.total_tokens)} tokens · ${usd(
        facets.data.total_cost_usd
      )}`
    : facets.error
    ? "Totals unavailable"
    : "Loading…";

  return (
    <div className="panes">
      <div className="pane-detail">
        {openSession ? (
          <SessionDetail
            session={openSession}
            onBack={() => setOpenId(undefined)}
          />
        ) : (
          <>
        <div className="toolbar">
          {/* The placeholder carries the one piece of query syntax that is not
              guessable — words are AND'ed, so quoting is the only way to ask
              for a phrase — and the tooltip carries the rest. It sits on the
              wrapper because `Search` takes no `title`, and widening a shared
              component for one caller's hint is the larger change. */}
          <div
            style={{ width: 260, display: "flex" }}
            title={
              "Words are matched together, in any order. " +
              'Wrap text in "double quotes" to match it as a phrase. ' +
              "The last word matches as a prefix while you type."
            }
          >
            <Search
              value={query}
              onChange={onQuery}
              placeholder={'Search — "quotes" match a phrase'}
            />
          </div>

          <Dropdown
            small
            className="sess-select"
            label={
              filters.project
                ? projectLabel(projectOptions, filters.project)
                : "All projects"
            }
            value={filters.project}
            onChange={(v) => patchFilters({ project: v })}
            options={[
              { value: "", label: "All projects" },
              // The server filters on project_path — the decoded path, exactly.
              // encoded_name silently matches nothing.
              ...projectOptions.map((p) => ({
                value: p.decoded_path,
                label: `${p.decoded_path} (${p.session_count})`,
              })),
            ]}
          />

          {/* Relevance is offered only while a term is active, because without
              a MATCH there is no rank — the server would answer `recent` and
              the label would be a lie. The value shown is the resolved sort,
              so "Sort: Relevance" appears the moment a search selects it. */}
          <Dropdown
            small
            className="sess-select"
            label={`Sort: ${
              SORTS.find((s) => s.value === sort)?.label ?? "Recent"
            }`}
            value={sort}
            onChange={(v) => patchFilters({ sort: v as Sort })}
            options={SORTS.filter(
              (s) => s.value !== "relevance" || searching
            ).map((s) => ({ value: s.value, label: s.label }))}
          />

          <button
            className={`iconbtn ${filters.favorites ? "iconbtn--active" : ""}`}
            title={filters.favorites ? "Showing favourites" : "Favourites only"}
            aria-pressed={filters.favorites}
            onClick={() => patchFilters({ favorites: !filters.favorites })}
          >
            <Icon name="star" size={14} />
          </button>

          <div className="spacer" />

          {scanning && status.data && (
            <>
              <span className="sess-scan">
                <span className="sess-spinner" />
                <span className="toolbar__sub tnum">
                  Scanning {integer(status.data.files_done)}/
                  {integer(status.data.files_total)}
                </span>
              </span>
              <div className="toolbar__sep" />
            </>
          )}

          <span className="toolbar__sub tnum">{summary}</span>
          <div className="toolbar__sep" />
          <button
            className="iconbtn"
            title="Rescan ~/.claude"
            onClick={refresh}
            disabled={scanning}
          >
            <Icon name="refresh" size={14} />
          </button>
        </div>

        {page.error && items.length === 0 ? (
          <Empty
            icon="alert"
            title="Couldn't load sessions"
            text={page.error}
            action={
              <button className="btn" onClick={reloadList}>
                <Icon name="refresh" size={13} />
                Try again
              </button>
            }
          />
        ) : page.loading && items.length === 0 ? (
          <div className="sess-loading">
            <span className="sess-spinner" />
            Loading sessions…
          </div>
        ) : items.length === 0 ? (
          neverScanned(status.data) ? (
            <Empty
              icon="database"
              title="No scan has run yet"
              text="Agento has not indexed ~/.claude yet, so there is nothing to list. Run a scan to build the session index."
              action={
                <button className="btn" onClick={refresh} disabled={scanning}>
                  <Icon name="refresh" size={13} />
                  Scan now
                </button>
              }
            />
          ) : filtersActive ? (
            <Empty
              icon="search"
              title="No matching sessions"
              text="No indexed session matches the current search and filters."
              action={
                <button
                  className="btn"
                  onClick={() => {
                    setQuery("");
                    patchFilters(INITIAL_FILTERS);
                  }}
                >
                  Clear filters
                </button>
              }
            />
          ) : (
            <Empty
              icon="history"
              title="No sessions indexed"
              text="The last scan found no Claude Code sessions under the configured config directories."
              action={
                <button className="btn" onClick={refresh} disabled={scanning}>
                  <Icon name="refresh" size={13} />
                  Rescan
                </button>
              }
            />
          )
        ) : (
          <div className="scroll" style={{ flex: 1, minHeight: 0 }}>
            <table className="table table--striped sess-table">
              <thead>
                <tr>
                  <th style={{ width: "40%" }}>Session</th>
                  <th style={{ width: 190 }}>Project · Branch</th>
                  <th style={{ width: 74 }}>Mode</th>
                  <th className="num" style={{ width: 56 }}>
                    Msgs
                  </th>
                  <th style={{ width: 150 }}>Tokens in / out</th>
                  <th className="num" style={{ width: 84 }}>
                    Cost
                  </th>
                  <th className="num" style={{ width: 82 }}>
                    Last
                  </th>
                </tr>
              </thead>
              <tbody>
                {groups.map(([group, rows]) => (
                  <Fragment key={group}>
                    <tr className="rowgroup">
                      <td colSpan={7}>
                        {group} · {rows.length}{" "}
                        {rows.length === 1 ? "session" : "sessions"}
                      </td>
                    </tr>
                    {rows.map((s) => {
                      const badge = modeBadge(s);
                      const tin = tokensIn(s);
                      const tout = tokensOut(s);
                      const fill = Math.max(
                        2,
                        Math.min(100, ((tin + tout) / meterScale) * 100)
                      );
                      return (
                        <tr
                          key={s.session_id}
                          className={
                            s.session_id === selectedId ? "is-selected" : ""
                          }
                          onClick={() => select(s)}
                          onDoubleClick={() => {
                            select(s);
                            setOpenId(s.session_id);
                          }}
                        >
                          <td title={s.display_title}>
                            <span className="sess-title">
                              {s.is_favorite && (
                                <Icon
                                  name="star"
                                  size={11}
                                  className="sess-star"
                                />
                              )}
                              <span className="sess-title__text">
                                {s.display_title || "Untitled session"}
                              </span>
                            </span>
                            <MatchSnippet snippet={s.match_snippet} />
                          </td>
                          <td title={s.project_path}>
                            <span
                              className="mono"
                              style={{ color: "var(--fg-secondary)" }}
                            >
                              {tildePath(s.project_path)}
                            </span>{" "}
                            {s.git_branch && (
                              <span className="sess-dim">{s.git_branch}</span>
                            )}
                          </td>
                          <td>
                            {badge && (
                              <span className={`badge ${badge.tone}`}>
                                {badge.label}
                              </span>
                            )}
                          </td>
                          <td className="num tnum">{integer(s.message_count)}</td>
                          <td>
                            <span className="sess-tokens">
                              <span className="meter">
                                <span
                                  className="meter__fill"
                                  style={{ width: `${fill}%` }}
                                />
                              </span>
                              <span
                                className="tnum"
                                style={{ fontSize: "var(--text-sm)" }}
                              >
                                {compactNumber(tin)} / {compactNumber(tout)}
                              </span>
                            </span>
                          </td>
                          <td className="num tnum">{usd(totalCost(s))}</td>
                          <td
                            className="num"
                            style={{ color: "var(--fg-tertiary)" }}
                            title={dateTime(s.last_activity)}
                          >
                            {relativeTime(s.last_activity)}
                          </td>
                        </tr>
                      );
                    })}
                  </Fragment>
                ))}

                {hasMore && (
                  <tr className="sess-more">
                    <td colSpan={7}>
                      <button
                        className="btn btn--ghost"
                        onClick={loadMore}
                        disabled={page.loading}
                      >
                        {loadingMore ? (
                          <>
                            <span className="sess-spinner" />
                            Loading…
                          </>
                        ) : (
                          <>
                            <Icon name="arrowDown" size={13} />
                            Load more
                            {remaining > 0 ? ` (${integer(remaining)} left)` : ""}
                          </>
                        )}
                      </button>
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        )}

        {(page.error && items.length > 0) || refreshError ? (
          <div className="sess-err">
            <Icon name="alert" size={13} />
            <span className="sess-err__msg">
              {refreshError ?? page.error}
            </span>
            <div className="spacer" />
            <button
              className="btn btn--ghost"
              onClick={() => {
                setRefreshError(undefined);
                reloadList();
              }}
            >
              Retry
            </button>
          </div>
        ) : null}
          </>
        )}
      </div>

      {inspectorOpen && (
        <>
          <Splitter variable="--inspector-w" min={220} max={420} invert />
          <aside className="pane-inspector">
            <div className="inspector__head">Session</div>
            <div className="inspector__scroll scroll">
              {selected ? (
                <Inspector
                  session={selected}
                  busy={busy}
                  error={actionError}
                  continuedChat={continuedChat}
                  onOpen={(s) => setOpenId(s.session_id)}
                  onToggleFavorite={toggleFavorite}
                  onContinue={continueInChat}
                />
              ) : (
                <div className="sess-note">No session selected.</div>
              )}
            </div>
          </aside>
        </>
      )}
    </div>
  );
}

/* --- Inspector ------------------------------------------------------------ */

function Inspector({
  session,
  busy,
  error,
  continuedChat,
  onOpen,
  onToggleFavorite,
  onContinue,
}: {
  session: ClaudeSessionSummary;
  busy: "favorite" | "continue" | undefined;
  error: string | undefined;
  continuedChat: string | undefined;
  onOpen(s: ClaudeSessionSummary): void;
  onToggleFavorite(s: ClaudeSessionSummary): void;
  onContinue(s: ClaudeSessionSummary): void;
}) {
  const usage = usageOf(session.usage);
  const sub = usageOf(session.subagent_usage);
  const cost = costOf(session.cost);
  const subCost = costOf(session.subagent_cost);
  const prs = session.prs ?? [];
  const badge = modeBadge(session);
  const agentName = sessionAgentName(session);

  return (
    <>
      <InspGroup title="Session">
        <div className="sess-heading selectable">
          {session.display_title || "Untitled session"}
        </div>
        {session.preview && (
          <div className="sess-preview selectable">{session.preview}</div>
        )}
        <InspRow label="ID">
          <span className="mono selectable">{session.session_id}</span>
        </InspRow>
        {session.custom_title && (
          <InspRow label="Custom">{session.custom_title}</InspRow>
        )}
        {session.ai_title && session.ai_title !== session.display_title && (
          <InspRow label="AI title">{session.ai_title}</InspRow>
        )}
        {session.native_title && (
          <InspRow label="Native">{session.native_title}</InspRow>
        )}
        {/* Another name Claude Code recorded for the session, shown only when
            it is not one of the titles above — the same suppression the AI
            title gets, for the reasons in lib/sessionAgent.ts. It was labelled
            "Agent" and sat beside Model and Config until it turned out never to
            name an agent, so it belongs here with the other titles. */}
        {agentName && <InspRow label="Named">{agentName}</InspRow>}
        <InspRow label="Project">
          <span title={session.project_path}>
            {tildePath(session.project_path)}
          </span>
        </InspRow>
        {session.cwd && session.cwd !== session.project_path && (
          <InspRow label="Directory">{tildePath(session.cwd)}</InspRow>
        )}
        {session.relocated_cwd && (
          <InspRow label="Relocated">{tildePath(session.relocated_cwd)}</InspRow>
        )}
        <InspRow label="Branch">{session.git_branch || "—"}</InspRow>
        {session.worktree_name && (
          <InspRow label="Worktree">
            {session.worktree_name}
            {session.worktree_branch ? ` · ${session.worktree_branch}` : ""}
          </InspRow>
        )}
        {session.original_branch && (
          <InspRow label="From">{session.original_branch}</InspRow>
        )}
        <InspRow label="Model">{session.model || "—"}</InspRow>
        <InspRow label="Mode">
          {badge ? (
            <span className={`badge ${badge.tone}`}>{badge.label}</span>
          ) : (
            "—"
          )}
        </InspRow>
        <InspRow label="Config">{session.config_dir ?? "Default"}</InspRow>
      </InspGroup>

      <InspGroup title="Activity">
        <InspRow label="Started">{dateTime(session.start_time)}</InspRow>
        <InspRow label="Last">{dateTime(session.last_activity)}</InspRow>
        <InspRow label="Active">
          <span className="tnum">{duration(session.active_duration_ms)}</span>
        </InspRow>
        <InspRow label="Sub-agents">
          <span className="tnum">
            {duration(session.subagent_active_duration_ms)}
          </span>
        </InspRow>
        <InspRow label="Total">
          <span className="tnum">{duration(totalDuration(session))}</span>
        </InspRow>
        <InspRow label="Messages">
          <span className="tnum">{integer(session.message_count)}</span>
        </InspRow>
        <InspRow label="Events">
          <span className="tnum">{integer(session.event_count)}</span>
        </InspRow>
        <InspRow label="Compactions">
          <span className="tnum">{integer(session.compaction_count)}</span>
        </InspRow>
        {session.dropped_tokens > 0 && (
          <InspRow label="Dropped">
            <span className="tnum">{integer(session.dropped_tokens)}</span>
          </InspRow>
        )}
      </InspGroup>

      <InspGroup title="Tokens">
        <InspRow label="Input">
          <span className="tnum">{integer(usage.input_tokens)}</span>
        </InspRow>
        <InspRow label="Output">
          <span className="tnum">{integer(usage.output_tokens)}</span>
        </InspRow>
        <InspRow label="Cache read">
          <span className="tnum">{integer(usage.cache_read_tokens)}</span>
        </InspRow>
        <InspRow label="Cache write">
          <span className="tnum">{integer(usage.cache_creation_tokens)}</span>
        </InspRow>
        <InspRow label="Billable">
          <span className="tnum">
            {integer(tokensIn(session))} in / {integer(tokensOut(session))} out
          </span>
        </InspRow>
      </InspGroup>

      <InspGroup title={`Sub-agents · ${integer(session.subagent_count)}`}>
        <InspRow label="Input">
          <span className="tnum">{integer(sub.input_tokens)}</span>
        </InspRow>
        <InspRow label="Output">
          <span className="tnum">{integer(sub.output_tokens)}</span>
        </InspRow>
        <InspRow label="Cache read">
          <span className="tnum">{integer(sub.cache_read_tokens)}</span>
        </InspRow>
        <InspRow label="Cache write">
          <span className="tnum">{integer(sub.cache_creation_tokens)}</span>
        </InspRow>
        <InspRow label="Cost">
          <span className="tnum">{usd(subCost.total_usd)}</span>
        </InspRow>
      </InspGroup>

      <InspGroup title="Cost">
        <InspRow label="Input">
          <span className="tnum">{usd(cost.input_usd)}</span>
        </InspRow>
        <InspRow label="Output">
          <span className="tnum">{usd(cost.output_usd)}</span>
        </InspRow>
        <InspRow label="Cache read">
          <span className="tnum">{usd(cost.cache_read_usd)}</span>
        </InspRow>
        <InspRow label="Cache write">
          <span className="tnum">{usd(cost.cache_write_usd)}</span>
        </InspRow>
        <InspRow label="Session">
          <span className="tnum">{usd(cost.total_usd)}</span>
        </InspRow>
        <InspRow label="Sub-agents">
          <span className="tnum">{usd(subCost.total_usd)}</span>
        </InspRow>
        <InspRow label="Total">
          <span className="tnum" style={{ color: "var(--green)" }}>
            {usd(totalCost(session))}
          </span>
        </InspRow>
        {session.unpriced_models?.length ? (
          <InspRow label="Unpriced">
            <span title={session.unpriced_models.join(", ")}>
              {session.unpriced_models.length} models ·{" "}
              {compactNumber(session.unpriced_tokens ?? 0)} tokens
            </span>
          </InspRow>
        ) : null}
      </InspGroup>

      {prs.length > 0 && (
        <InspGroup title={`Pull requests · ${prs.length}`}>
          <div className="sess-prs">
            {prs.map((pr) => (
              <div className="sess-pr" key={`${pr.pr_repository}#${pr.pr_number}`}>
                <a
                  href={pr.pr_url}
                  onClick={(e) => {
                    e.preventDefault();
                    openExternal(pr.pr_url);
                  }}
                >
                  #{pr.pr_number}
                </a>
                <span className="sess-pr__repo">{pr.pr_repository}</span>
              </div>
            ))}
          </div>
        </InspGroup>
      )}

      <InspGroup title="Actions">
        <div className="sess-actions">
          <button className="btn btn--primary" onClick={() => onOpen(session)}>
            <Icon name="chat" size={13} />
            View session
          </button>
          <button
            className="btn"
            disabled={busy !== undefined}
            onClick={() => onToggleFavorite(session)}
          >
            <Icon
              name="star"
              size={13}
              className={session.is_favorite ? "sess-fav--on" : undefined}
            />
            {session.is_favorite ? "Remove favourite" : "Add to favourites"}
          </button>
          <button
            className="btn"
            disabled={busy !== undefined}
            onClick={() => onContinue(session)}
          >
            <Icon name="play" size={13} />
            {busy === "continue" ? "Starting…" : "Continue in chat"}
          </button>
          <button
            className="btn"
            onClick={() => navigator.clipboard?.writeText(session.session_id)}
          >
            <Icon name="copy" size={13} />
            Copy session ID
          </button>
          {continuedChat !== undefined && (
            <div className="sess-note">
              Continued as chat <span className="mono">{continuedChat}</span> —
              open Chats to resume it.
            </div>
          )}
          {error && <div className="sess-note sess-note--error">{error}</div>}
        </div>
      </InspGroup>
    </>
  );
}

/* --- Toolbar dropdown ----------------------------------------------------- */

function projectLabel(options: ClaudeProject[], value: string): string {
  const hit = options.find((p) => p.decoded_path === value);
  return hit ? hit.decoded_path : value;
}
