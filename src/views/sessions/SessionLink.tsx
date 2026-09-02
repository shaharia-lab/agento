/**
 * A session, rendered as a control wherever one is *named* (#536).
 *
 * Session affordances used to live only in `views/SessionsView.tsx`, so a
 * session named anywhere else — the three *Top sessions* rankings, a continued
 * chat's provenance banner — was inert text and the one action those panels
 * exist for (go and look at that session) was the one thing they could not do.
 *
 * Two things are shared from here, and the split matters:
 *
 * - **`sessionMenuItems` is the menu**, and it is the single definition of it.
 *   `SessionsView`'s own rows build their menu from this function too, so the
 *   five entries cannot drift into two lists that agree only by inspection.
 *   What each entry *does* is the caller's, because the list view patches its
 *   loaded page and reloads its facets where this component has neither.
 * - **`SessionLink` is the control** — the button, the right-click, and the
 *   four id-only actions a surface that knows nothing but an id can perform.
 *
 * Opening is a **hand-off, not a second detail renderer**: `navigate("sessions",
 * { sessionId })` lands the user in the section that owns sessions, so the back
 * button, the inspector and every row action they find there are the real ones.
 *
 * The stylesheet is imported here rather than declared in `styles/sessions.css`
 * — the `components/charts.tsx` / `SaveBar.tsx` shape — because the consumers
 * are in sections that do not import that sheet, so a rule there would reach
 * them only for as long as `App.tsx` happens to import `SessionsView`
 * statically.
 */
import { useCallback, useMemo, useState } from "react";
import { api, qs } from "../../lib/api";
import { copyText } from "../../lib/clipboard";
import { describeError } from "../../lib/hooks";
import { useNavigate } from "../../lib/nav";
import type { ClaudeSessionSummary, SessionPage } from "../../lib/types";
import { ContextMenu, type ContextMenuItem } from "../../components/ui";
import "../../styles/sessionlink.css";

export interface SessionMenuSpec {
  sessionId: string;
  /**
   * The row's `project_path`. Absent where the calling surface has no path for
   * the session — "Copy project path" is then disabled rather than copying an
   * empty string, which reads exactly like a successful copy.
   */
  projectPath?: string;
  /**
   * `undefined` means **not yet known**, which is not the same as "not a
   * favourite": offering *Add to favourites* for something already starred is
   * a wrong label on a destructive-ish toggle. The item is then disabled and
   * labelled neutrally.
   */
  isFavorite?: boolean;
  /** Any of this session's actions already in flight. */
  busy: boolean;
  onView(): void;
  onToggleFavorite(): void;
  onContinue(): void;
  onCopy(what: string, value: string): void;
}

/**
 * The five entries a session's context menu has, everywhere it has one.
 *
 * One definition rather than one per surface: a second hand-written array is
 * what drifts, and the labels here are load-bearing (the favourite item's
 * *label* is a function of the row, so a copy goes stale silently).
 */
export function sessionMenuItems(spec: SessionMenuSpec): ContextMenuItem[] {
  return [
    {
      label: "View session",
      icon: "chat",
      onSelect: spec.onView,
    },
    {
      label:
        spec.isFavorite === undefined
          ? "Favourite"
          : spec.isFavorite
          ? "Remove favourite"
          : "Add to favourites",
      icon: "star",
      disabled: spec.busy || spec.isFavorite === undefined,
      onSelect: spec.onToggleFavorite,
    },
    {
      label: "Continue in chat",
      icon: "play",
      disabled: spec.busy,
      onSelect: spec.onContinue,
    },
    {
      label: "Copy session ID",
      icon: "copy",
      onSelect: () => spec.onCopy("session ID", spec.sessionId),
    },
    {
      label: "Copy project path",
      icon: "copy",
      disabled: !spec.projectPath,
      onSelect: () => spec.onCopy("project path", spec.projectPath ?? ""),
    },
  ];
}

export function SessionLink({
  sessionId,
  title,
  project,
}: {
  sessionId: string;
  /** What to show. Falls back to the id, which is what the rankings do. */
  title?: string;
  /**
   * A *display* path from the calling surface. Analytics ranks on
   * `decoded_path` while the sessions list keys on `project_path` literally —
   * a documented divergence — so the hydrated row's own `project_path` wins
   * for "Copy project path" as soon as it lands.
   */
  project?: string;
}) {
  const navigate = useNavigate();
  const [at, setAt] = useState<{ x: number; y: number }>();
  const [row, setRow] = useState<ClaudeSessionSummary>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  const open = useCallback(
    () => navigate("sessions", { sessionId }),
    [navigate, sessionId]
  );

  /**
   * Resolve the row lazily, on right-click, purely for the favourite's label.
   *
   * Through the **list** rather than `GET /claude-sessions/{id}`: that route
   * reads the transcript back and answers with every message in it, which is
   * megabytes to learn one boolean, while `add_search`'s LIKE half covers
   * `session_id` so this is a cheap summary read. A miss is not an error — the
   * session may have left the corpus — and every id-only item still works.
   */
  const hydrate = useCallback(async () => {
    try {
      const page = await api.get<SessionPage>(
        `/claude-sessions${qs({ q: sessionId, limit: 5 })}`
      );
      const hit = (page.items ?? []).find((s) => s.session_id === sessionId);
      if (hit) setRow(hit);
    } catch {
      // Deliberately silent: the menu is usable without it.
    }
  }, [sessionId]);

  const toggleFavorite = useCallback(async () => {
    if (!row) return;
    const next = !row.is_favorite;
    setBusy(true);
    setError(undefined);
    try {
      await api.patch<void>(`/claude-sessions/${sessionId}`, {
        is_favorite: next,
      });
      setRow({ ...row, is_favorite: next });
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }, [row, sessionId]);

  /**
   * Create the resuming chat and **go to it** — #485's rule. Reporting the new
   * id in place is what made a success and a 404 look identical.
   */
  const continueInChat = useCallback(async () => {
    setBusy(true);
    setError(undefined);
    try {
      const res = await api.post<{ chat_id: string }>(
        `/claude-sessions/${sessionId}/continue`
      );
      if (!res?.chat_id) throw new Error("the server returned no chat id");
      navigate("chats", { chatId: res.chat_id });
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }, [navigate, sessionId]);

  /** Only a failure is reported; the menu closing is the acknowledgement. */
  const copyValue = useCallback(async (what: string, value: string) => {
    if (await copyText(value)) return;
    setError(`Could not copy the ${what} to the clipboard.`);
  }, []);

  const items = useMemo(
    () =>
      sessionMenuItems({
        sessionId,
        projectPath: row?.project_path || project || undefined,
        isFavorite: row ? !!row.is_favorite : undefined,
        busy,
        onView: open,
        onToggleFavorite: toggleFavorite,
        onContinue: continueInChat,
        onCopy: copyValue,
      }),
    [sessionId, row, project, busy, open, toggleFavorite, continueInChat, copyValue]
  );

  const label = title || sessionId;

  return (
    <>
      <button
        type="button"
        className="sess-link"
        title={label}
        onClick={open}
        // `stopPropagation` as well as `preventDefault`: this button sits inside
        // rows and banners that may have menus of their own, and a session's
        // menu must win over its container's.
        onContextMenu={(e) => {
          e.preventDefault();
          e.stopPropagation();
          setError(undefined);
          setAt({ x: e.clientX, y: e.clientY });
          if (!row) void hydrate();
        }}
      >
        {label}
      </button>
      {/* A `span`, not a `div`: this component is used inside phrasing content
          (the Chats banner's `.resumed__id`), where a block element is invalid
          nesting. `display: block` in the stylesheet does the layout. */}
      {error && <span className="sess-link__error">{error}</span>}
      {at && (
        <ContextMenu at={at} items={items} onClose={() => setAt(undefined)} />
      )}
    </>
  );
}
