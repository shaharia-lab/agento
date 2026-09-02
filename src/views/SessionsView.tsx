import {
  Fragment,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { api, qs } from "../lib/api";
import { CopyButton } from "../components/CopyButton";
import type {
  ClaudeProject,
  ClaudeSessionDetail,
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
import { useNavigate } from "../lib/nav";
import { snippetParts, snippetText } from "../lib/snippet";
import { sessionAgentName } from "../lib/sessionAgent";
import { openExternal } from "../lib/tauri";
import { copyText } from "../lib/clipboard";
import {
  ContextMenu,
  type ContextMenuItem,
  Dropdown,
  Empty,
  InspGroup,
  InspRow,
  Search,
  Splitter,
} from "../components/ui";
import { SessionDetail } from "./sessions/SessionDetail";
import { sessionMenuItems } from "./sessions/SessionLink";
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

/**
 * An inclusive numeric bound, held as the **input's own strings**.
 *
 * `""` and `0` are different answers — unbounded versus "at most zero" — and a
 * number-typed state would collapse them. One min/max pair also expresses every
 * comparison the list needs: min alone reads "at least", max alone "at most",
 * both "between", so no operator control sits beside each field.
 */
interface Range {
  min: string;
  max: string;
}

const NO_RANGE: Range = { min: "", max: "" };

function rangeSet(r: Range): boolean {
  return r.min.trim() !== "" || r.max.trim() !== "";
}

/** Whether a session must have a linked PR, must have none, or either. */
type LinkFilter = "" | "with" | "without";

interface Filters {
  project: string;
  /**
   * One Claude account, identified by the config dir its sessions were scanned
   * from. `""` is every account.
   *
   * Indexing every configured dir into one corpus is deliberate — analytics is
   * retrospective, and a machine running two accounts wants both in every
   * total — so this is the only way to read one account's sessions on their
   * own. It is a *view* over the corpus, not the hidden-project mechanism in
   * Settings: nothing is excluded from reporting by picking one here.
   */
  configDir: string;
  /** The ordering while no search term is active. */
  sort: Sort;
  /**
   * The ordering while one is — `""` meaning "follow the server", which for a
   * search is relevance.
   *
   * **Two slots rather than one, because the sort a search wants and the sort a
   * listing wants are different questions.** With a single field, "restore the
   * previous sort when the query clears" has nowhere to restore *from*, and the
   * control becomes one-way: relevance is what an unchosen sort already resolves
   * to while searching, so recording that pick would store a `relevance` that
   * reads as `recent` the moment the term goes, silently discarding whatever the
   * user had picked before. Keeping the browsing sort untouched by anything done
   * during a search is what makes both directions work with no effect, no
   * remembered transition and no extra request.
   */
  searchSort: Sort | "";
  favorites: boolean;

  /* --- The advanced set, edited as a draft in the Filters panel ----------- */

  /** `""` matches every mode; otherwise the raw `permission_mode` column. */
  permissionMode: string;
  /** `""` matches every model. */
  model: string;
  links: LinkFilter;
  /** Main-thread messages only — the column the Msgs cell renders. */
  messages: Range;
  /** *Active* minutes, parent plus sub-agents; never the wall-clock span. */
  duration: Range;
  tokensIn: Range;
  tokensOut: Range;
  /** USD. */
  cost: Range;
  /** `YYYY-MM-DD` as the date inputs hold it; `""` is unbounded. */
  from: string;
  to: string;
}

const INITIAL_FILTERS: Filters = {
  project: "",
  configDir: "",
  sort: "recent",
  searchSort: "",
  favorites: false,
  permissionMode: "",
  model: "",
  links: "",
  messages: NO_RANGE,
  duration: NO_RANGE,
  tokensIn: NO_RANGE,
  tokensOut: NO_RANGE,
  cost: NO_RANGE,
  from: "",
  to: "",
};

/**
 * How many of the advanced filters are narrowing the list.
 *
 * A min/max pair counts once and the date range counts once, so the number
 * matches the fields the panel shows rather than the parameters the request
 * carries — the badge is answering "how much is hidden behind this button".
 */
function advancedCount(f: Filters): number {
  let n = 0;
  if (f.permissionMode) n += 1;
  if (f.model) n += 1;
  if (f.links) n += 1;
  for (const r of [f.messages, f.duration, f.tokensIn, f.tokensOut, f.cost]) {
    if (rangeSet(r)) n += 1;
  }
  if (f.from || f.to) n += 1;
  return n;
}

/**
 * The parameter names `sessions/query.rs::SessionQuery::parse` reads, spelled
 * exactly as it reads them.
 *
 * A closed union rather than `Record<string, …>` because a misspelled parameter
 * is **silent on both sides**: the server ignores what it does not recognise
 * and answers the unfiltered set, so a `permission_mode` typed `permissionMode`
 * would look like a filter that simply matches everything. There is no
 * TypeScript test harness here, so making the compiler reject the typo is the
 * only guard available. Keep it in step with that parser.
 */
type FilterParamKey =
  | "q"
  | "project"
  | "config_dir"
  | "favorites"
  | "permission_mode"
  | "model"
  | "links"
  | "messages_min"
  | "messages_max"
  | "duration_min"
  | "duration_max"
  | "tokens_in_min"
  | "tokens_in_max"
  | "tokens_out_min"
  | "tokens_out_max"
  | "cost_min"
  | "cost_max"
  | "from"
  | "to";

type FilterParams = Partial<Record<FilterParamKey, string | number | boolean>>;

/**
 * Serialize a filter set into request parameters.
 *
 * One function for three callers — the list, the facet aggregate beside it, and
 * the panel's pending count — because they must describe the *same* set. Two
 * call sites narrowing by two slightly different query strings is exactly how a
 * row count and the total printed above it come to disagree.
 *
 * An unset filter is **absent**, never an empty value: `qs()` drops both, but
 * the distinction is the server's — `model=` would ask for a model named `""`.
 */
function filterParamsOf(search: string, f: Filters): FilterParams {
  return {
    q: search.trim() || undefined,
    project: f.project || undefined,
    config_dir: f.configDir || undefined,
    favorites: f.favorites ? true : undefined,
    permission_mode: f.permissionMode || undefined,
    model: f.model || undefined,
    links: f.links || undefined,

    messages_min: bound(f.messages.min),
    messages_max: bound(f.messages.max),
    // The filter's unit is minutes and the column's is milliseconds; the server
    // scales the bound rather than the column (`add_range(…, 60_000.0)`), so
    // what goes on the wire is minutes.
    duration_min: bound(f.duration.min),
    duration_max: bound(f.duration.max),
    tokens_in_min: bound(f.tokensIn.min),
    tokens_in_max: bound(f.tokensIn.max),
    tokens_out_min: bound(f.tokensOut.min),
    tokens_out_max: bound(f.tokensOut.max),
    cost_min: bound(f.cost.min),
    cost_max: bound(f.cost.max),

    from: dayBoundary(f.from, "start"),
    to: dayBoundary(f.to, "end"),
  };
}

/** One side of a range, sent verbatim — the server ignores what it cannot parse. */
function bound(v: string): string | undefined {
  return v.trim() || undefined;
}

/**
 * A facet's option list with the current selection appended when the facet no
 * longer offers it.
 *
 * The escape hatch every option-set control here needs: without it a control
 * disappears at the one moment it is the only thing that could clear itself,
 * leaving the list pinned to a value nothing on screen mentions.
 */
function withSelected(
  options: string[] | null | undefined,
  selected: string
): string[] {
  const list = options?.filter(Boolean) ?? [];
  return selected && !list.includes(selected) ? [...list, selected] : list;
}

/**
 * A `YYYY-MM-DD` date input as the RFC 3339 instant the server parses.
 *
 * The bound is taken in the **local** zone, and the `to` side is the *end* of
 * that day: the server compares `start_time <= to`, so a `to` of midnight would
 * exclude every session that ran on the very day the user picked. `from` is the
 * start of its day against `last_activity >= from`, so the range reads as the
 * two dates inclusive.
 *
 * Anything that is not a real date yields `undefined` rather than a plausible
 * wrong instant — the server ignores an unparseable bound too, so both sides
 * agree it is unbounded. The shape check is not enough on its own: `Date`
 * *normalizes* rather than rejecting, so `2026-02-30` would otherwise be sent
 * as 2 March. Only the round trip catches that.
 */
function dayBoundary(day: string, edge: "start" | "end"): string | undefined {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(day.trim());
  if (!m) return undefined;
  const [year, month, date] = [Number(m[1]), Number(m[2]) - 1, Number(m[3])];
  const t =
    edge === "start"
      ? new Date(year, month, date, 0, 0, 0, 0)
      : new Date(year, month, date, 23, 59, 59, 999);
  const real =
    t.getFullYear() === year && t.getMonth() === month && t.getDate() === date;
  return real ? t.toISOString() : undefined;
}

/**
 * The sort a request will really run under — `sessions/query.rs::resolve_sort`,
 * mirrored so the control cannot claim an ordering the server would not use.
 *
 * Two rules, both the server's:
 *
 * * **No explicit choice plus a search term is `relevance`.** Somebody who typed
 *   a query wants the best match first; somebody who did not has no ranking to
 *   sort by. An explicit pick always wins, so a user who chose "Recent" for this
 *   search keeps it while typing.
 * * **`relevance` with no search term is `recent`**, because without a `MATCH`
 *   there is no rank.
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

/**
 * The permission modes Claude Code writes, and how each reads.
 *
 * One table for the row badge and the Mode filter, so the option a user picks
 * is spelled exactly as the column they picked it from. An unknown value keeps
 * its raw text in both places rather than being dropped — the corpus predates
 * some of these names.
 */
const MODE_LABELS: Record<string, string> = {
  bypassPermissions: "Bypass",
  plan: "Plan",
  acceptEdits: "Accept",
  dontAsk: "Don't ask",
  default: "Default",
};

const MODE_TONES: Record<string, string> = {
  bypassPermissions: "badge--amber",
  plan: "badge--purple",
  acceptEdits: "badge--teal",
  dontAsk: "badge--teal",
};

/**
 * `permission_mode` is a free-form column — the scanner copies whatever the
 * transcript's `permission-mode` event carried, with no enum in between — so
 * every read of these tables goes through a `typeof` check rather than `??`.
 * A plain object inherits `toString`, `constructor` and `valueOf`, and `??`
 * does not catch a function: the value would reach JSX as a React child and as
 * a `className`. The `switch` these tables replaced was immune by construction.
 */
function lookup(table: Record<string, string>, key: string): string | undefined {
  const v = table[key];
  return typeof v === "string" ? v : undefined;
}

function modeLabel(mode: string): string {
  return lookup(MODE_LABELS, mode) ?? mode;
}

function modeBadge(s: ClaudeSessionSummary): { label: string; tone: string } | null {
  // `omitempty` on the Go side, so the field is absent on a row that recorded
  // no mode — which is not the same as one whose mode this table does not know.
  const mode = s.permission_mode ?? "";
  const label = lookup(MODE_LABELS, mode);
  if (label) return { label, tone: lookup(MODE_TONES, mode) ?? "" };
  return s.mode ? { label: s.mode, tone: "" } : null;
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

/* --- The advanced filter panel ------------------------------------------- */

const LINK_LABELS: Record<LinkFilter, string> = {
  "": "Any",
  with: "With a linked PR",
  without: "Without a linked PR",
};

function FilterField({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="sess-filters__field">
      <div className="sess-filters__label">
        {label}
        {hint && <span className="sess-filters__hint">{hint}</span>}
      </div>
      {children}
    </div>
  );
}

/**
 * One inclusive min/max pair. Both sides are plain `<input>` text, so a
 * part-typed value is never coerced — and nothing here fires a request, because
 * the panel edits a draft.
 */
function RangeField({
  label,
  hint,
  step,
  value,
  onChange,
}: {
  label: string;
  hint?: string;
  step?: string;
  value: Range;
  onChange(r: Range): void;
}) {
  return (
    <FilterField label={label} hint={hint}>
      <div className="sess-filters__range">
        <input
          className="sess-filters__num tnum"
          type="number"
          min="0"
          step={step}
          inputMode="decimal"
          placeholder="Min"
          aria-label={`${label} minimum`}
          value={value.min}
          onChange={(e) => onChange({ ...value, min: e.target.value })}
        />
        <span className="sess-filters__dash">–</span>
        <input
          className="sess-filters__num tnum"
          type="number"
          min="0"
          step={step}
          inputMode="decimal"
          placeholder="Max"
          aria-label={`${label} maximum`}
          value={value.max}
          onChange={(e) => onChange({ ...value, max: e.target.value })}
        />
      </div>
    </FilterField>
  );
}

/**
 * How many sessions the *draft* would match, so Apply is not a leap in the
 * dark — the facet aggregate is the cheap half of a search (~101 ms against the
 * page's seconds on a common term), and this asks for nothing else.
 *
 * It is a child component so the request exists only while the panel is open,
 * rather than doubling facet traffic for every user who never opens it. The
 * params object is referentially stable between draft edits, so debouncing it
 * directly is what keeps a keystroke from becoming a request.
 */
function DraftMatchCount({ params }: { params: FilterParams }) {
  const settled = useDebounced(params, 300);
  const preview = useResource<SessionFacets>(
    (signal) =>
      api.get<SessionFacets>(`/claude-sessions/facets${qs(settled)}`, signal),
    [settled]
  );

  // A failed read knows nothing about how many rows match, and saying "0" about
  // a request that never landed would read as an answer.
  if (preview.error && !preview.data) {
    return <span className="sess-filters__count">Count unavailable</span>;
  }
  if (!preview.data) {
    return <span className="sess-filters__count">Counting…</span>;
  }
  const n = preview.data.total;
  return (
    <span className="sess-filters__count tnum">
      {integer(n)} {n === 1 ? "session" : "sessions"} match
    </span>
  );
}

export function SessionsView({
  inspectorOpen,
  openSessionId,
  openSessionNonce,
}: {
  inspectorOpen: boolean;
  /** A session handed off from another section, to open on arrival (#536). */
  openSessionId?: string;
  /** `App`'s nav nonce, so the *same* hand-off twice still fires. */
  openSessionNonce?: number;
}) {
  const [query, setQuery] = useState("");
  const q = useDebounced(query, 250);
  const [filters, setFilters] = useState<Filters>(INITIAL_FILTERS);

  /**
   * What the Filters panel edits, applied to `filters` on the button.
   *
   * Deliberately not live. Every change here resets the keyset cursor and
   * discards every accumulated page, so typing `50` into a minimum would run a
   * query for `5` and throw the list away and back mid-keystroke — a server
   * round trip per character, not a client-side re-filter. The pending count
   * beside Apply is what keeps that from being a leap in the dark.
   */
  const [draft, setDraft] = useState<Filters>(INITIAL_FILTERS);
  const [panelOpen, setPanelOpen] = useState(false);

  // The debounced term, not the raw one: gating relevance on what the user is
  // still typing would flicker the option in and out per keystroke and refetch
  // the list ahead of the debounce it exists to respect.
  const searching = q.trim() !== "";
  const sort = resolveSort(
    searching ? filters.searchSort : filters.sort,
    searching
  );

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

  const filterParams = useMemo(() => filterParamsOf(q, filters), [q, filters]);

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
    // The panel's draft carries the *whole* filter set, so a toolbar control
    // writing only `filters` would make Apply silently revert it. Patching both
    // is what keeps the two halves of one filter set from drifting apart.
    setDraft((d) => ({ ...d, ...patch }));
  }, []);

  /** Applied and draft together — the empty state's escape hatch. */
  const clearFilters = useCallback(() => {
    setQuery("");
    setPaging({ key: "", cursor: "" });
    setFilters(INITIAL_FILTERS);
    setDraft(INITIAL_FILTERS);
  }, []);

  const applyDraft = useCallback(() => {
    setPaging({ key: "", cursor: "" });
    setFilters(draft);
  }, [draft]);

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

  /**
   * The loaded rows, readable from an effect that must not *depend* on them —
   * the hand-off below fires on a nonce, and adding `items` to its deps would
   * re-open the handed-off session on every later page load.
   */
  const itemsRef = useRef(items);
  useEffect(() => {
    itemsRef.current = items;
  }, [items]);

  /** Why a hand-off did not open, when it did not. */
  const [handoffError, setHandoffError] = useState<string>();

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
    // ...except while a transcript is open. That session is legitimately
    // absent from the page — handed off from another section (#536), or simply
    // filtered out while it was being read — and stealing the selection here
    // also drops the `lastSelected` fallback `openSession` resolves through,
    // leaving the pane with nothing to render.
    if (selectedId && selectedId === openId) return;
    if (items.some((s) => s.session_id === selectedId)) return;
    select(items[0]);
  }, [items, selectedId, openId, select]);

  /**
   * A hand-off from another section: open the named session's transcript.
   *
   * Keyed on the **nonce**, not on `items`. The page is usually still loading
   * on arrival, so waiting for the row would leave a blank pane, and re-running
   * on every later page change would re-open a session the user had already
   * navigated away from. `App` clears `navTarget` on any navigation carrying
   * none, so a stale id cannot be re-applied on a later visit.
   *
   * The row comes from the loaded page when it happens to be there, else from
   * the by-id route — `ClaudeSessionDetail` **extends** `ClaudeSessionSummary`,
   * so the answer is a row `SessionDetail` can render.
   */
  useEffect(() => {
    const id = openSessionId;
    if (!id) return;
    setHandoffError(undefined);
    const known = itemsRef.current.find((s) => s.session_id === id);
    if (known) {
      select(known);
      setOpenId(id);
      return;
    }
    let cancelled = false;
    api
      .get<ClaudeSessionDetail>(`/claude-sessions/${id}`)
      .then((row) => {
        if (cancelled) return;
        select(row);
        setOpenId(id);
      })
      .catch((err) => {
        // A hand-off that silently does nothing is the bug #485 was filed for,
        // so the list says why it is still showing the list.
        if (!cancelled) setHandoffError(describeError(err));
      });
    return () => {
      cancelled = true;
    };
  }, [openSessionId, openSessionNonce, select]);

  /* --- Row actions -------------------------------------------------------- */

  const [busy, setBusy] = useState<"favorite" | "continue">();
  /**
   * Which action failed, not just what it said. The two surfaces render it in
   * different places — the inspector under its own three buttons, the full
   * session view under its one — so a favourite failure must not surface as
   * text under a "Continue in chat" button that was never pressed.
   */
  const [actionError, setActionError] =
    useState<{ action: "favorite" | "continue" | "copy"; message: string }>();
  const navigate = useNavigate();

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
        setActionError({ action: "favorite", message: describeError(err) });
      } finally {
        setBusy(undefined);
      }
    },
    [applyPatch, reloadFacets]
  );

  /**
   * Create the resuming chat and **go to it**.
   *
   * Reporting the new `chat_id` in place was the whole defect (#485): the note
   * rendered at the bottom of a scrolling inspector, so a success and a 404
   * looked identical — like nothing happening. Navigating is also what guards
   * a double-click: this view unmounts, so the second click has no button to
   * land on, and `busy` covers the window before that.
   */
  const continueInChat = useCallback(
    async (s: ClaudeSessionSummary) => {
      setBusy("continue");
      setActionError(undefined);
      try {
        const res = await api.post<{ chat_id: string }>(
          `/claude-sessions/${s.session_id}/continue`
        );
        // A 201 with no id is not a success to navigate on — it would land on
        // an empty Chats view, which is the symptom this issue is about.
        if (!res?.chat_id) throw new Error("the server returned no chat id");
        navigate("chats", { chatId: res.chat_id });
      } catch (err) {
        setActionError({ action: "continue", message: describeError(err) });
      } finally {
        setBusy(undefined);
      }
    },
    [navigate]
  );

  /**
   * Only a *failure* is reported. A successful copy is silent because that is
   * what a right-click Copy does everywhere else, and the menu closing is the
   * acknowledgement; a failure is not silent because `copyText` can genuinely
   * refuse under WebKitGTK (see its own note), and a menu item that looks the
   * same either way is one the user only finds out about by pasting.
   */
  const copyValue = useCallback(async (what: string, value: string) => {
    if (await copyText(value)) return;
    setActionError({
      action: "copy",
      message: `Could not copy the ${what} to the clipboard.`,
    });
  }, []);

  useEffect(() => {
    setActionError(undefined);
  }, [selectedId]);

  /* --- Row context menu ---------------------------------------------------- */

  /**
   * The row is held by **id**, not by value: the list reloads on a poll and a
   * favourite toggle patches it in place, so a captured row would let the
   * menu's own "Add / Remove favourite" label go stale against the state it is
   * about to flip.
   */
  const [menu, setMenu] = useState<{ at: { x: number; y: number }; id: string }>();
  const closeMenu = useCallback(() => setMenu(undefined), []);
  const menuSession = useMemo(
    () => (menu ? items.find((s) => s.session_id === menu.id) : undefined),
    [menu, items]
  );

  // The five entries come from `sessionMenuItems`, shared with `SessionLink`
  // (#536): this view and every surface that merely *names* a session must
  // offer the same menu, and a second hand-written array is what drifts. What
  // each entry does stays here — the list patches its loaded page and reloads
  // its facets, which a link rendered elsewhere has neither of.
  const menuItems = useMemo<ContextMenuItem[]>(() => {
    const s = menuSession;
    if (!s) return [];
    return sessionMenuItems({
      sessionId: s.session_id,
      projectPath: s.project_path,
      isFavorite: !!s.is_favorite,
      busy: busy !== undefined,
      onView: () => setOpenId(s.session_id),
      onToggleFavorite: () => toggleFavorite(s),
      onContinue: () => continueInChat(s),
      onCopy: copyValue,
    });
  }, [menuSession, busy, toggleFavorite, continueInChat, copyValue]);

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

  /**
   * The accounts the corpus actually spans, for the account control.
   *
   * The facet is computed over every *visible* session rather than the filtered
   * set, so picking one account never removes the others from the list — a
   * dropdown that drops the option you just chose cannot be un-chosen.
   *
   * A selection is kept in the list even when the facet no longer offers it,
   * which happens when the dir is un-indexed in Settings while it is picked.
   * Without this the control disappears at the moment it is the only thing that
   * could clear itself, leaving the list pinned to an account with no rows.
   */
  const accountOptions = useMemo(
    () => withSelected(facets.data?.config_dirs, filters.configDir),
    [facets.data, filters.configDir]
  );

  /**
   * The Mode and Model option sets, on the same terms as `accountOptions`: the
   * facet is computed over every *visible* session rather than the filtered
   * one, so picking a model never removes the others.
   *
   * A live selection keeps its own option even when the facet stops offering
   * it. That is not the filter narrowing the facet — it cannot — but the corpus
   * moving underneath a selection: a rescan dropping the last session of a
   * model, a project hidden in Settings, or a config dir leaving the indexed
   * set. Without it the dropdown loses the only entry that could clear it.
   */
  const modeOptions = useMemo(
    () => withSelected(facets.data?.permission_modes, draft.permissionMode),
    [facets.data, draft.permissionMode]
  );

  const modelOptions = useMemo(
    () => withSelected(facets.data?.models, draft.model),
    [facets.data, draft.model]
  );

  const activeCount = advancedCount(filters);
  const draftParams = useMemo(() => filterParamsOf(q, draft), [q, draft]);
  const draftDiffers = useMemo(
    () => JSON.stringify(draft) !== JSON.stringify(filters),
    [draft, filters]
  );

  const filtersActive =
    Boolean(
      query.trim() || filters.project || filters.configDir || filters.favorites
    ) || activeCount > 0;
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
            onContinue={continueInChat}
            continuing={busy === "continue"}
            continueError={
              actionError?.action === "continue" ? actionError.message : undefined
            }
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
              "Searches session titles and message content. " +
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

          {/* Only when there is a choice to make. One account is the common
              case, and a control whose every state shows the same rows is
              noise — hence driven by the facet rather than always rendered and
              disabled. The second clause is the escape hatch: a live selection
              always keeps its own control on screen, whatever the facet says. */}
          {(accountOptions.length > 1 || filters.configDir !== "") && (
            <Dropdown
              small
              className="sess-select"
              label={
                filters.configDir
                  ? tildePath(filters.configDir)
                  : "All accounts"
              }
              value={filters.configDir}
              onChange={(v) => patchFilters({ configDir: v })}
              options={[
                { value: "", label: "All accounts" },
                // The server matches `config_dir` exactly, so the value stays
                // the absolute path the scan recorded; only the label is
                // shortened.
                ...accountOptions.map((d) => ({
                  value: d,
                  label: tildePath(d),
                })),
              ]}
            />
          )}

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
            onChange={(v) =>
              // Written into the slot the current mode reads, so a sort picked
              // for a search never outlives it and a sort picked while browsing
              // is never what a search silently inherits.
              patchFilters(
                searching ? { searchSort: v as Sort } : { sort: v as Sort }
              )
            }
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

          {/* The eight advanced filters live behind this, collapsed by default:
              the toolbar is already dense at 28px rows, and most listings are
              narrowed by search and project alone. The badge is the applied
              count, not the draft's — it answers "how much of what I am looking
              at is hidden behind this button". */}
          <button
            className={`btn sess-filters__toggle ${
              panelOpen ? "sess-filters__toggle--on" : ""
            }`}
            aria-expanded={panelOpen}
            aria-controls="sess-filters-panel"
            title="Advanced filters"
            onClick={() => setPanelOpen((v) => !v)}
          >
            <Icon name="filter" size={13} />
            Filters
            {activeCount > 0 && (
              <span className="sess-filters__badge tnum">{activeCount}</span>
            )}
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

        {panelOpen && (
          <div className="sess-filters" id="sess-filters-panel">
            <div className="sess-filters__grid">
              {/* Both dropdowns follow the account control's rule: rendered
                  only when there is a choice to make, and always while one is
                  selected. */}
              {(modeOptions.length > 1 || draft.permissionMode !== "") && (
                <FilterField label="Mode">
                  <Dropdown
                    small
                    className="sess-filters__select"
                    ariaLabel="Permission mode"
                    label={
                      draft.permissionMode
                        ? modeLabel(draft.permissionMode)
                        : "Any mode"
                    }
                    value={draft.permissionMode}
                    onChange={(v) => setDraft((d) => ({ ...d, permissionMode: v }))}
                    options={[
                      { value: "", label: "Any mode" },
                      ...modeOptions.map((m) => ({
                        value: m,
                        label: modeLabel(m),
                      })),
                    ]}
                  />
                </FilterField>
              )}

              {(modelOptions.length > 1 || draft.model !== "") && (
                <FilterField label="Model">
                  <Dropdown
                    small
                    className="sess-filters__select"
                    ariaLabel="Model"
                    label={draft.model || "Any model"}
                    value={draft.model}
                    onChange={(v) => setDraft((d) => ({ ...d, model: v }))}
                    options={[
                      { value: "", label: "Any model" },
                      ...modelOptions.map((m) => ({ value: m, label: m })),
                    ]}
                  />
                </FilterField>
              )}

              {/* Nothing in the corpus is linked to a PR on most machines, and
                  a control whose every state shows the same rows is noise. */}
              {(facets.data?.has_prs || draft.links !== "") && (
                <FilterField label="Linked PRs">
                  <Dropdown
                    small
                    className="sess-filters__select"
                    ariaLabel="Linked pull requests"
                    label={LINK_LABELS[draft.links]}
                    value={draft.links}
                    onChange={(v) =>
                      setDraft((d) => ({ ...d, links: v as LinkFilter }))
                    }
                    options={(["", "with", "without"] as LinkFilter[]).map(
                      (v) => ({ value: v, label: LINK_LABELS[v] })
                    )}
                  />
                </FilterField>
              )}

              <RangeField
                label="Messages"
                // SQL_MESSAGE_COUNT is bare `c.message_count`, where cost,
                // duration and the token ranges all fold sub-agents in. That is
                // deliberate on both sides — each filter matches the column its
                // own cell renders — but it has to be said, or a "Messages"
                // filter would silently promise a total it does not use.
                hint="Main thread only"
                value={draft.messages}
                onChange={(r) => setDraft((d) => ({ ...d, messages: r }))}
              />

              <RangeField
                label="Active minutes"
                // Active duration, not the wall-clock span: a resumed session's
                // span counts every idle day between sittings, which is exactly
                // why the column the list sorts and renders is not that one.
                hint="Active time, not elapsed"
                value={draft.duration}
                onChange={(r) => setDraft((d) => ({ ...d, duration: r }))}
              />

              <RangeField
                label="Tokens in"
                hint="Incl. sub-agents"
                value={draft.tokensIn}
                onChange={(r) => setDraft((d) => ({ ...d, tokensIn: r }))}
              />

              <RangeField
                label="Tokens out"
                hint="Incl. sub-agents"
                value={draft.tokensOut}
                onChange={(r) => setDraft((d) => ({ ...d, tokensOut: r }))}
              />

              <RangeField
                label="Cost"
                hint="USD, incl. sub-agents"
                step="0.01"
                value={draft.cost}
                onChange={(r) => setDraft((d) => ({ ...d, cost: r }))}
              />

              <FilterField
                label="Date range"
                hint="Active on or after / started on or before"
              >
                <div className="sess-filters__range">
                  <input
                    className="sess-filters__date"
                    type="date"
                    aria-label="Active on or after"
                    value={draft.from}
                    max={draft.to || undefined}
                    onChange={(e) =>
                      setDraft((d) => ({ ...d, from: e.target.value }))
                    }
                  />
                  <span className="sess-filters__dash">–</span>
                  <input
                    className="sess-filters__date"
                    type="date"
                    aria-label="Started on or before"
                    value={draft.to}
                    min={draft.from || undefined}
                    onChange={(e) =>
                      setDraft((d) => ({ ...d, to: e.target.value }))
                    }
                  />
                </div>
              </FilterField>
            </div>

            <div className="sess-filters__foot">
              <DraftMatchCount params={draftParams} />
              <div className="spacer" />
              <button
                className="btn"
                disabled={!filtersActive && !draftDiffers}
                onClick={clearFilters}
              >
                Clear all
              </button>
              <button
                className="btn btn--primary"
                disabled={!draftDiffers}
                onClick={applyDraft}
              >
                Apply
              </button>
            </div>
          </div>
        )}

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
                <button className="btn" onClick={clearFilters}>
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
                          // Select first, so the inspector is showing the row
                          // the menu is about to act on. `main.tsx` already
                          // suppresses the webview's own menu on chrome, but
                          // this preventDefault is what makes a row's menu
                          // unconditional — it wins even where a selection
                          // elsewhere on the page would have let the native one
                          // through.
                          onContextMenu={(e) => {
                            e.preventDefault();
                            select(s);
                            setMenu({
                              at: { x: e.clientX, y: e.clientY },
                              id: s.session_id,
                            });
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

        {/* A hand-off (#536) that could not resolve its session says so, rather
            than dropping the user on an unexplained list. */}
        {handoffError ? (
          <div className="sess-err">
            <Icon name="alert" size={13} />
            <span className="sess-err__msg">
              That session could not be opened: {handoffError}
            </span>
            <div className="spacer" />
            <button
              className="btn btn--ghost"
              onClick={() => setHandoffError(undefined)}
            >
              Dismiss
            </button>
          </div>
        ) : null}

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
            {/* Outside `.inspector__scroll` on purpose: as the last group of a
                scrolling pane these three were below the fold on any session
                with a full metadata block, which is the whole of #486. */}
            {selected && (
              <div className="sess-strip">
                <div className="sess-strip__row">
                  <button
                    className="btn btn--primary sess-strip__btn"
                    title="View session"
                    aria-label="View session"
                    onClick={() => setOpenId(selected.session_id)}
                  >
                    <Icon name="chat" size={13} />
                  </button>
                  <button
                    className="btn sess-strip__btn"
                    disabled={busy !== undefined}
                    // `!!` because the field is `omitempty`: a session that is
                    // not a favourite has no `is_favorite` key at all, and
                    // `undefined` makes React drop the attribute rather than
                    // render "false" — a toggle that only announces its state
                    // in one of its two states.
                    aria-pressed={!!selected.is_favorite}
                    title={
                      selected.is_favorite
                        ? "Remove favourite"
                        : "Add to favourites"
                    }
                    aria-label={
                      selected.is_favorite
                        ? "Remove favourite"
                        : "Add to favourites"
                    }
                    onClick={() => toggleFavorite(selected)}
                  >
                    <Icon
                      name="star"
                      size={13}
                      className={
                        selected.is_favorite ? "sess-fav--on" : undefined
                      }
                    />
                  </button>
                  <button
                    className="btn sess-strip__btn"
                    disabled={busy !== undefined}
                    title={
                      busy === "continue" ? "Starting…" : "Continue in chat"
                    }
                    aria-label="Continue in chat"
                    onClick={() => continueInChat(selected)}
                  >
                    <Icon name="play" size={13} />
                  </button>
                </div>
                {/* A successful continue navigates away and a successful copy
                    is silent, so the only thing left to render is a failure —
                    directly under the buttons, where it is now always in view. */}
                {actionError && (
                  <div className="sess-note sess-note--error">
                    {actionError.message}
                  </div>
                )}
              </div>
            )}
            <div className="inspector__scroll scroll">
              {selected ? (
                <Inspector session={selected} />
              ) : (
                <div className="sess-note">No session selected.</div>
              )}
            </div>
          </aside>
        </>
      )}

      {menu && menuSession && (
        <ContextMenu at={menu.at} items={menuItems} onClose={closeMenu} />
      )}
    </div>
  );
}

/* --- Inspector ------------------------------------------------------------ */

/**
 * The metadata groups alone. The actions live in `.sess-strip` above the
 * scrolling pane this renders into, so nothing here takes a handler.
 */
function Inspector({ session }: { session: ClaudeSessionSummary }) {
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
        <div className="sess-heading">
          {session.display_title || "Untitled session"}
        </div>
        {session.preview && (
          <div className="sess-preview">{session.preview}</div>
        )}
        {/* Both of these are single values a user copies whole and neither
            fits the pane, so the button carries the real string while the row
            shows an abbreviated one (#469). */}
        <InspRow label="ID">
          <span className="row insp-row__copy">
            <span className="mono truncate">{session.session_id}</span>
            <CopyButton text={session.session_id} title="Copy session ID" />
          </span>
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
          <span className="row insp-row__copy">
            <span className="truncate" title={session.project_path}>
              {tildePath(session.project_path)}
            </span>
            <CopyButton
              text={session.project_path}
              title="Copy the project path"
            />
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

    </>
  );
}

/* --- Toolbar dropdown ----------------------------------------------------- */

function projectLabel(options: ClaudeProject[], value: string): string {
  const hit = options.find((p) => p.decoded_path === value);
  return hit ? hit.decoded_path : value;
}
