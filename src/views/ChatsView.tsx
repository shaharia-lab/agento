import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../lib/api";
import { describeError, useResource } from "../lib/hooks";
import {
  compactNumber,
  groupByRecency,
  initials,
  relativeTime,
  tildePath,
  toneFor,
  dateTime,
  usd,
} from "../lib/format";
import { Icon } from "../lib/icons";
import type {
  Agent,
  ChatDetail,
  ChatMessage,
  ChatSession,
  ClaudeMessage,
  ClaudeSessionDetail,
} from "../lib/types";
import {
  Empty,
  InspGroup,
  InspRow,
  Search,
  Segmented,
  Splitter,
} from "../components/ui";
import {
  NewChatBar,
  NEW_CHAT_INITIAL,
  PERMISSION_LABELS,
} from "./chat/NewChatBar";
import { saveNewChatPrefs, type NewChatPrefs } from "../lib/newChatPrefs";
import { Composer } from "./chat/Composer";
import { Transcript } from "./chat/Transcript";
import { useChatStream } from "./chat/useChatStream";
import { SessionTranscript } from "./sessions/SessionTranscript";
import "../styles/chats.css";

type Filter = "all" | "favorites" | "running";

/**
 * The detail endpoint answers `{session, messages}` rather than a flattened
 * session, so it is folded into ChatDetail here — the rest of the view only
 * ever sees the flat shape.
 */
async function fetchDetail(id: string, signal: AbortSignal): Promise<ChatDetail> {
  const raw = await api.get<Partial<ChatDetail> & { session?: ChatSession }>(
    `/chats/${id}`,
    signal
  );
  const session = raw.session ?? (raw as ChatSession);
  return { ...session, messages: raw.messages ?? [] };
}

export function ChatsView({
  inspectorOpen,
  newChatNonce = 0,
  openChatId,
  openChatNonce = 0,
}: {
  inspectorOpen: boolean;
  newChatNonce?: number;
  /** A chat another view handed over; applied when `openChatNonce` moves. */
  openChatId?: string;
  openChatNonce?: number;
}) {
  const chats = useResource<ChatSession[]>(
    (signal) => api.get<ChatSession[]>("/chats", signal),
    []
  );
  const agents = useResource<Agent[]>(
    (signal) => api.get<Agent[]>("/agents", signal),
    []
  );

  const [selected, setSelected] = useState<string | null>(null);
  const [drafting, setDrafting] = useState(false);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [draft, setDraft] = useState("");
  /**
   * How the *next* chat will be created. Held as one record rather than five
   * `useState`s so `NewChatBar` can resolve its defaults in a single edit —
   * five separate setters would each fire a render with the others still
   * unresolved, and the working directory would flicker through empty.
   */
  const [newChat, setNewChat] = useState<NewChatPrefs>(NEW_CHAT_INITIAL);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState("");
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [actionError, setActionError] = useState<string>();

  /** Turns produced in this session, kept per chat until the server copy loads. */
  const [extra, setExtra] = useState<Record<string, ChatMessage[]>>({});

  const detail = useResource<ChatDetail | null>(
    (signal) => (selected ? fetchDetail(selected, signal) : Promise.resolve(null)),
    [selected]
  );

  const chatsReload = chats.reload;
  const stream = useChatStream(
    useCallback(
      (chatId: string, message: ChatMessage | null) => {
        if (message) {
          setExtra((prev) => ({
            ...prev,
            [chatId]: [...(prev[chatId] ?? []), message],
          }));
        }
        chatsReload();
      },
      [chatsReload]
    )
  );

  const streamingId = useRef<string | null>(null);
  streamingId.current = stream.chatId;

  // A fresh server transcript replaces the optimistic one — but never while
  // that chat is mid-turn, or the turn in progress would vanish.
  const lastDetail = useRef<ChatDetail | null>(null);
  useEffect(() => {
    const data = detail.data;
    if (!data || data === lastDetail.current) return;
    lastDetail.current = data;
    if (streamingId.current === data.id) return;
    setExtra((prev) => (prev[data.id]?.length ? { ...prev, [data.id]: [] } : prev));
  }, [detail.data]);

  // Select the most recent conversation once, the way a mail client does.
  const autoSelected = useRef(false);
  useEffect(() => {
    if (autoSelected.current || drafting || selected) return;
    const first = chats.data?.[0];
    if (!first) return;
    autoSelected.current = true;
    setSelected(first.id);
  }, [chats.data, drafting, selected]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return (chats.data ?? []).filter((c) => {
      if (filter === "favorites" && !c.is_favorite) return false;
      if (filter === "running" && c.id !== stream.chatId) return false;
      if (!q) return true;
      return (
        chatTitle(c).toLowerCase().includes(q) ||
        c.agent_slug.toLowerCase().includes(q) ||
        c.working_directory.toLowerCase().includes(q)
      );
    });
  }, [chats.data, query, filter, stream.chatId]);

  const grouped = useMemo(
    () => groupByRecency(filtered, (c) => c.updated_at),
    [filtered]
  );

  // useResource keeps the previous payload while the next one loads; ignoring
  // it until the ids agree stops the old transcript showing under a new header.
  const loaded = detail.data?.id === selected ? detail.data : null;

  const listRow = chats.data?.find((c) => c.id === selected);
  const session: ChatSession | null = loaded
    ? { ...loaded, ...(listRow ?? {}) }
    : listRow ?? null;

  const messages = useMemo(
    () => [
      ...(loaded?.messages ?? []),
      ...(selected ? extra[selected] ?? [] : []),
    ],
    [loaded, extra, selected]
  );

  const busy = stream.chatId !== null;
  const agentLabel = session?.agent_slug || "Agento";

  // --- The conversation a continued chat resumes (#490) --------------------
  //
  // "Continue in chat" records the source session on the chat row; the history
  // itself stays in the Claude transcript, because importing it would duplicate
  // megabytes per session and corrupt this chat's own usage figures, which are
  // stored rather than derived. So it is fetched through the same live
  // session-detail read the Sessions view uses, and rendered read-only above the
  // chat's own turns.
  const resumedFrom = session?.continued_from_session_id ?? "";
  const resumedPath = session?.continued_from_project_path ?? "";
  const resumedCount = session?.continued_from_message_count ?? 0;
  // `resumedPath` is in the deps even though the request does not carry it: two
  // chats can be continued from the same session id under *different* project
  // paths, and without it the second reuses the first's answer and evaluates
  // the mismatch below against a payload fetched for the other pair.
  const resumed = useResource<ClaudeSessionDetail | null>(
    (signal) =>
      resumedFrom
        ? api.get<ClaudeSessionDetail>(`/claude-sessions/${resumedFrom}`, signal)
        : Promise.resolve(null),
    [resumedFrom, resumedPath]
  );

  // `useResource` keeps the previous payload while the next one loads — the same
  // reason `loaded` above ignores `detail.data` until the ids agree. Continue is
  // idempotent per source session now, so moving between two continued chats is
  // ordinary, and without this the *previous* source's transcript renders under
  // the new chat's header, cut at the new chat's boundary. `error` is stale on
  // the same terms: a failed read for one chat would otherwise be reported as
  // the next one's.
  const resumedData =
    resumed.data?.session_id === resumedFrom ? resumed.data : null;
  const resumedError = resumedData ? undefined : resumed.error;

  // **The corpus keys a session on the pair, and so does the reference** (#492).
  // Migration 37 records `(continued_from_session_id, continued_from_project_path)`
  // for exactly that reason — the #362 family — but `GET /api/claude-sessions/{id}`
  // resolves through `find_session_file`, which is first-match-by-id across every
  // config dir and project. So the same id under two project paths answers with
  // *a* transcript, not necessarily the recorded one, and every check above it
  // passes. Compare the pair and refuse to render a transcript that contradicts
  // it: a different project's conversation shown as this chat's history is a
  // silent wrong answer, which is worse than an honest note.
  //
  // Two rules the comparison depends on. It is gated on a **non-empty** recorded
  // path, because a chat may legitimately carry the id with no path (the Rust
  // side omits an empty string from the wire) and there is then nothing to
  // contradict. And both sides are compared **raw**: `continue_chat.rs` stores
  // `detail.summary.project_path` verbatim and both values come from the same
  // `walk::decode_project_path`, so for a dash-encoded project both are the
  // encoded name — normalising or tilde-shortening either side would invent
  // mismatches. `tildePath` is for display only.
  const resumedMismatch = useMemo(
    () =>
      resumedData && resumedPath && resumedData.project_path !== resumedPath
        ? { recorded: resumedPath, found: resumedData.project_path }
        : null,
    [resumedData, resumedPath]
  );

  // **A fixed prefix, and that is the whole guard against a double render.**
  // The CLI appends a resumed turn to the *same* transcript file, so once this
  // chat has taken a turn the source session contains those messages too —
  // rendering the whole transcript here would show the newest turn once from the
  // transcript and once from `chat_messages`. `continued_from_message_count` was
  // recorded when the chat was created and never moves.
  //
  // `slice` clamps on its own, so a transcript that was compacted or rewritten
  // to fewer messages renders what is left rather than indexing past the end.
  const inherited = useMemo<ClaudeMessage[]>(
    () =>
      resumedData && !resumedMismatch
        ? (resumedData.messages ?? []).slice(0, resumedCount)
        : [],
    [resumedData, resumedMismatch, resumedCount]
  );

  const select = useCallback((id: string) => {
    setSelected(id);
    setDrafting(false);
    setRenaming(null);
    setConfirmDelete(false);
    setActionError(undefined);
  }, []);

  const startNew = useCallback(() => {
    setSelected(null);
    setDrafting(true);
    setConfirmDelete(false);
    setActionError(undefined);
    autoSelected.current = true;
  }, []);

  // The sidebar button, the native menu and ⌘N all land here: App bumps the
  // nonce, and the view answers by opening a draft. 0 is "never asked".
  const seenNonce = useRef(0);
  useEffect(() => {
    if (newChatNonce === seenNonce.current) return;
    seenNonce.current = newChatNonce;
    startNew();
  }, [newChatNonce, startNew]);

  // Sessions → "Continue in chat" hands a freshly created chat over (#485), the
  // same way the nonce above hands over "open a draft".
  //
  // The id is applied without waiting for `/chats` to resolve: `detail` fetches
  // by id, so the transcript and the composer are live immediately and the row
  // simply highlights once the list arrives. `autoSelected` is claimed so the
  // "select the most recent conversation" effect cannot overwrite the hand-off
  // when that list lands.
  const seenOpenNonce = useRef(0);
  const [composerFocus, setComposerFocus] = useState(0);
  useEffect(() => {
    if (openChatNonce === seenOpenNonce.current) return;
    seenOpenNonce.current = openChatNonce;
    if (!openChatId) return;
    autoSelected.current = true;
    select(openChatId);
    // "Ready to type" is the point of the hand-off, so the caret goes to the
    // composer — which mounts in this same commit, since `selected` is what
    // renders it.
    setComposerFocus((n) => n + 1);
  }, [openChatId, openChatNonce, select]);

  const send = useCallback(async () => {
    const content = draft.trim();
    if (!content || busy) return;

    let id = selected;
    if (!id) {
      // An agent run without a working directory lands in whatever directory
      // the server happened to start in — refuse to create the chat instead.
      const workingDir = newChat.workingDir.trim();
      if (!workingDir) {
        setActionError("Set a working directory before starting the chat.");
        return;
      }
      try {
        const created = await api.post<ChatSession>("/chats", {
          agent_slug: newChat.agentSlug,
          working_directory: workingDir,
          model: newChat.model,
          settings_profile_id: newChat.settingsProfileId,
          permission_mode: newChat.permissionMode,
        });
        id = created.id;
        setSelected(created.id);
        setDrafting(false);
        // Only a chat that was actually created is a preference — see
        // `saveNewChatPrefs`.
        saveNewChatPrefs({ ...newChat, workingDir });
        chats.reload();
      } catch (err) {
        setActionError(describeError(err));
        return;
      }
    }

    const chatId = id;
    setDraft("");
    stream.reset();
    setExtra((prev) => ({
      ...prev,
      [chatId]: [
        ...(prev[chatId] ?? []),
        { role: "user", content, timestamp: new Date().toISOString() },
      ],
    }));
    stream.start(chatId, content);
  }, [draft, busy, selected, newChat, chats, stream]);

  // The answer to an agent question travels over /input, not /messages, and
  // the server does not persist it — echoing it here keeps the exchange
  // readable ("what did I answer?") for the rest of the sitting.
  const answerPrompt = useCallback(
    (text: string) => {
      const chatId = stream.chatId;
      if (chatId) {
        setExtra((prev) => ({
          ...prev,
          [chatId]: [
            ...(prev[chatId] ?? []),
            { role: "user", content: text, timestamp: new Date().toISOString() },
          ],
        }));
      }
      void stream.answer(text);
    },
    [stream]
  );

  const patch = useCallback(
    async (id: string, body: { title?: string; is_favorite?: boolean }) => {
      try {
        await api.patch<ChatSession>(`/chats/${id}`, body);
        chats.reload();
      } catch (err) {
        setActionError(describeError(err));
      }
    },
    [chats]
  );

  const remove = useCallback(
    async (id: string) => {
      try {
        await api.del(`/chats/${id}`);
        setSelected(null);
        setConfirmDelete(false);
        autoSelected.current = false;
        chats.reload();
      } catch (err) {
        setActionError(describeError(err));
      }
    },
    [chats]
  );

  return (
    <div className="panes">
      {/* --- List pane ----------------------------------------------------- */}
      <div className="pane-list">
        <div className="listhead">
          <div className="listhead__row">
            <Search value={query} onChange={setQuery} placeholder="Search chats" />
            <button className="iconbtn" title="New chat" onClick={startNew}>
              <Icon name="plus" size={14} />
            </button>
          </div>
          <div className="listhead__row">
            <Segmented<Filter>
              value={filter}
              onChange={setFilter}
              options={[
                { value: "all", label: "All" },
                { value: "favorites", label: "Starred" },
                { value: "running", label: "Running" },
              ]}
            />
            <div className="spacer" />
            <button
              className="iconbtn"
              title="Refresh"
              onClick={() => chats.reload()}
            >
              <Icon name="refresh" size={14} />
            </button>
          </div>
        </div>

        {chats.error && (
          <div className="banner banner--error">
            <Icon name="alert" size={13} />
            <span className="truncate">{chats.error}</span>
            <div className="spacer" />
            <button className="btn" onClick={() => chats.reload()}>
              Retry
            </button>
          </div>
        )}

        <div className="list__scroll scroll">
          {chats.loading && !chats.data && <div className="listnote">Loading…</div>}

          {!chats.loading && !chats.error && chats.data?.length === 0 && (
            <div className="listnote">No chats yet</div>
          )}

          {chats.data?.length !== 0 && grouped.length === 0 && !chats.loading && (
            <div className="listnote">No chats match “{query}”</div>
          )}

          {grouped.map(([group, items]) => (
            <div key={group}>
              <div className="listgroup">{group}</div>
              {items.map((c) => (
                <button
                  key={c.id}
                  className={`listrow ${
                    c.id === selected ? "listrow--active" : ""
                  }`}
                  onClick={() => select(c.id)}
                >
                  <div className={`avatar avatar--${toneFor(c.agent_slug)}`}>
                    {c.agent_slug ? (
                      initials(c.agent_slug)
                    ) : (
                      <Icon name="chat" size={14} />
                    )}
                  </div>

                  <div className="listrow__body">
                    <div className="listrow__top">
                      <span className="listrow__title">{chatTitle(c)}</span>
                      <span className="listrow__time">
                        {relativeTime(c.updated_at)}
                      </span>
                    </div>
                    <div className="listrow__preview">
                      {c.working_directory
                        ? tildePath(c.working_directory)
                        : c.model || "No working directory"}
                    </div>
                    <div className="listrow__meta">
                      {c.id === stream.chatId && (
                        <span className="badge badge--green">
                          <span className="dot dot--green dot--pulse" />
                          Running
                        </span>
                      )}
                      {c.is_favorite && (
                        <Icon
                          name="star"
                          size={11}
                          style={{ color: "var(--amber)" }}
                        />
                      )}
                      <span>{c.agent_slug || "No agent"}</span>
                      <span>·</span>
                      <span className="tnum">{tokenLabel(c)}</span>
                    </div>
                  </div>
                </button>
              ))}
            </div>
          ))}
        </div>
      </div>

      <Splitter variable="--list-w" min={240} max={460} />

      {/* --- Detail pane --------------------------------------------------- */}
      <div className="pane-detail">
        {drafting ? (
          <>
            <div className="toolbar">
              <div className="avatar" style={{ width: 22, height: 22 }}>
                <Icon name="chat" size={12} />
              </div>
              <div className="toolbar__title">New conversation</div>
              <div className="spacer" />
              <button
                className="iconbtn"
                title="Cancel"
                onClick={() => setDrafting(false)}
              >
                <Icon name="close" size={14} />
              </button>
            </div>

            <NewChatBar
              agents={agents.data ?? []}
              agentsError={agents.error}
              value={newChat}
              onChange={setNewChat}
            />

            <div className="transcript scroll">
              <Empty
                icon="sparkle"
                title="Start the conversation"
                text="Choose how this conversation should run, then send the first message. The chat is created when you send — a working directory is required."
              />
            </div>

            {actionError && <ErrorBar text={actionError} onClose={() => setActionError(undefined)} />}

            <Composer
              value={draft}
              onChange={setDraft}
              onSend={send}
              busy={busy}
              stopping={stream.stopping}
              onStop={stream.stop}
              placeholder={`Message ${newChat.agentSlug || "Agento"}…`}
            />
          </>
        ) : !selected ? (
          <Empty
            icon="chat"
            title={chats.data?.length ? "No conversation selected" : "No chats yet"}
            text={
              chats.data?.length
                ? "Pick a chat from the list, or start a new one to talk to an agent."
                : "Start a conversation to put an agent to work."
            }
            action={
              <button className="btn btn--primary" onClick={startNew}>
                <Icon name="plus" size={13} />
                New chat
              </button>
            }
          />
        ) : detail.error && !loaded ? (
          <Empty
            icon="alert"
            title="Could not load this conversation"
            text={detail.error}
            action={
              <button className="btn" onClick={() => detail.reload()}>
                <Icon name="refresh" size={13} />
                Retry
              </button>
            }
          />
        ) : (
          <>
            <div className="toolbar">
              <div
                className={`avatar avatar--${toneFor(session?.agent_slug)}`}
                style={{ width: 22, height: 22 }}
              >
                {session?.agent_slug ? (
                  initials(session.agent_slug)
                ) : (
                  <Icon name="chat" size={12} />
                )}
              </div>

              {renaming === selected ? (
                <input
                  className="renamefield"
                  autoFocus
                  value={renameValue}
                  onChange={(e) => setRenameValue(e.target.value)}
                  onBlur={() => setRenaming(null)}
                  onKeyDown={(e) => {
                    if (e.key === "Escape") setRenaming(null);
                    if (e.key === "Enter" && renameValue.trim()) {
                      patch(selected, { title: renameValue.trim() });
                      setRenaming(null);
                    }
                  }}
                />
              ) : (
                <button
                  className="toolbar__title truncate renametrigger"
                  title="Rename"
                  onClick={() => {
                    setRenameValue(session ? chatTitle(session) : "");
                    setRenaming(selected);
                  }}
                >
                  {session ? chatTitle(session) : "Conversation"}
                </button>
              )}

              <div className="spacer" />
              {busy && (
                <span className="badge badge--green">
                  <span className="dot dot--green dot--pulse" />
                  Running
                </span>
              )}
              <div className="toolbar__sep" />
              <button
                className={`iconbtn ${session?.is_favorite ? "iconbtn--active" : ""}`}
                title={session?.is_favorite ? "Unstar" : "Star"}
                onClick={() =>
                  patch(selected, { is_favorite: !session?.is_favorite })
                }
              >
                <Icon name="star" size={14} />
              </button>
              <button
                className="iconbtn"
                title="Reload transcript"
                onClick={() => detail.reload()}
              >
                <Icon name="refresh" size={14} />
              </button>
              {confirmDelete ? (
                <button
                  className="btn btn--danger"
                  onClick={() => remove(selected)}
                  onBlur={() => setConfirmDelete(false)}
                  autoFocus
                >
                  Delete chat
                </button>
              ) : (
                <button
                  className="iconbtn"
                  title="Delete chat"
                  disabled={busy}
                  onClick={() => setConfirmDelete(true)}
                >
                  <Icon name="trash" size={14} />
                </button>
              )}
            </div>

            {!loaded && detail.loading ? (
              <div className="transcript scroll">
                <div className="listnote">Loading conversation…</div>
              </div>
            ) : messages.length === 0 && !resumedFrom && stream.chatId !== selected ? (
              <div className="transcript scroll">
                <Empty
                  icon="sparkle"
                  title="No messages yet"
                  text={`Send the first message to ${agentLabel}.`}
                />
              </div>
            ) : (
              <Transcript
                chatId={selected}
                messages={messages}
                agent={agentLabel}
                tone={toneFor(session?.agent_slug)}
                live={stream.chatId === selected ? stream.live : null}
                tools={stream.tools}
                prompt={stream.chatId === selected ? stream.prompt : null}
                streaming={stream.chatId === selected}
                onAnswer={answerPrompt}
                onDecide={(allow) => void stream.decide(allow)}
                header={
                  resumedFrom ? (
                    <ResumedHistory
                      sessionId={resumedFrom}
                      projectPath={resumedPath}
                      messages={inherited}
                      loading={resumed.loading && !resumedData}
                      error={resumedError}
                      mismatch={resumedMismatch}
                    />
                  ) : null
                }
              />
            )}

            {(stream.error || actionError) && (
              <ErrorBar
                text={stream.error ?? actionError ?? ""}
                onClose={() => {
                  stream.dismissError();
                  setActionError(undefined);
                }}
              />
            )}

            <Composer
              value={draft}
              onChange={setDraft}
              onSend={send}
              busy={busy}
              stopping={stream.stopping}
              onStop={stream.stop}
              placeholder={`Message ${agentLabel}…`}
              focusNonce={composerFocus}
              meta={
                <span className="composer__meta">
                  {session?.model || stream.system?.model || "Default model"}
                </span>
              }
            />
          </>
        )}
      </div>

      {/* --- Inspector ----------------------------------------------------- */}
      {inspectorOpen && session && !drafting && (
        <>
          <Splitter variable="--inspector-w" min={220} max={420} invert />
          <aside className="pane-inspector">
            <div className="inspector__head">Conversation</div>
            <div className="inspector__scroll scroll">
              <InspGroup title="Details">
                <InspRow label="Agent">{session.agent_slug || "None"}</InspRow>
                <InspRow label="Model">
                  {session.model || stream.system?.model || "Default"}
                </InspRow>
                {/* The stored choice, which is what the *next* turn will run
                    under. `stream.system.permissionMode` below reports what the
                    CLI is running under right now; they agree once a turn has
                    started, and differ for a chat created before this column
                    existed. */}
                <InspRow label="Permissions">
                  {PERMISSION_LABELS[session.permission_mode ?? ""] ??
                    session.permission_mode}
                </InspRow>
                <InspRow label="Messages">
                  <span className="tnum">{messages.length}</span>
                </InspRow>
                <InspRow label="Status">
                  {busy ? (
                    <span className="badge badge--green">Running</span>
                  ) : (
                    <span className="badge">Idle</span>
                  )}
                </InspRow>
                <InspRow label="Created">{dateTime(session.created_at)}</InspRow>
                <InspRow label="Updated">{relativeTime(session.updated_at)}</InspRow>
              </InspGroup>

              <InspGroup title="Usage">
                <InspRow label="Input">
                  <span className="tnum">
                    {compactNumber(session.total_input_tokens ?? 0)}
                  </span>
                </InspRow>
                <InspRow label="Output">
                  <span className="tnum">
                    {compactNumber(session.total_output_tokens ?? 0)}
                  </span>
                </InspRow>
                <InspRow label="Cache read">
                  <span className="tnum">
                    {compactNumber(session.total_cache_read_tokens ?? 0)}
                  </span>
                </InspRow>
                <InspRow label="Cache write">
                  <span className="tnum">
                    {compactNumber(session.total_cache_creation_tokens ?? 0)}
                  </span>
                </InspRow>
                <InspRow label="Total">
                  <span className="tnum">{tokenLabel(session)}</span>
                </InspRow>
                {stream.result?.costUsd !== undefined && (
                  <InspRow label="Last turn">
                    <span className="tnum" style={{ color: "var(--green)" }}>
                      {usd(stream.result.costUsd)}
                    </span>
                  </InspRow>
                )}
              </InspGroup>

              {stream.system && (
                <InspGroup title="Live session">
                  <InspRow label="Permissions">
                    {stream.system.permissionMode ?? "—"}
                  </InspRow>
                  <InspRow label="Tools">
                    <span className="tnum">{stream.system.tools.length}</span>
                  </InspRow>
                  {stream.result?.numTurns !== undefined && (
                    <InspRow label="Turns">
                      <span className="tnum">{stream.result.numTurns}</span>
                    </InspRow>
                  )}
                </InspGroup>
              )}

              <InspGroup title="Working directory">
                <div className="pathwell mono">
                  {session.working_directory
                    ? tildePath(session.working_directory)
                    : "Not set"}
                </div>
              </InspGroup>

              <InspGroup title="Identifiers">
                <InspRow label="Chat">
                  <span className="mono truncate">{session.id}</span>
                </InspRow>
                <InspRow label="SDK session">
                  <span className="mono truncate">
                    {session.sdk_session_id || "—"}
                  </span>
                </InspRow>
              </InspGroup>
            </div>
          </aside>
        </>
      )}
    </div>
  );
}

/**
 * The conversation this chat was continued from, read-only, above its own turns.
 *
 * Four states and all four are honest. It is *not* gated on loading before the
 * chat renders: the composer must be usable immediately, and a source transcript
 * that has been moved, deleted or made unreadable since must leave the chat
 * working — so a failure is a note in this region rather than an error for the
 * whole view.
 *
 * `mismatch` is its own state rather than an `error` (#492): that transcript read
 * fine, it simply belongs to another project, and "could not be read" would send
 * the reader debugging the wrong thing. Its arm is ordered **before** the empty
 * one, because a mismatch renders no messages either and "no longer available"
 * would be the wrong sentence for a session that is available elsewhere.
 */
function ResumedHistory({
  sessionId,
  projectPath,
  messages,
  loading,
  error,
  mismatch,
}: {
  sessionId: string;
  projectPath: string;
  messages: ClaudeMessage[];
  loading: boolean;
  error?: string;
  mismatch?: { recorded: string; found: string } | null;
}) {
  return (
    <div className="resumed">
      <div className="resumed__head">
        <Icon name="history" size={12} />
        <span className="truncate" title={projectPath || sessionId}>
          Continued from {projectPath ? tildePath(projectPath) : "a Claude session"}
        </span>
        <span className="resumed__id mono truncate">{sessionId}</span>
      </div>

      {loading ? (
        <div className="resumed__note">Reading the earlier conversation…</div>
      ) : error ? (
        <div className="resumed__note">
          The earlier conversation could not be read: {error}
        </div>
      ) : mismatch ? (
        <div className="resumed__note">
          This chat was continued from{" "}
          <span className="mono">{tildePath(mismatch.recorded)}</span>, but that
          session id resolved to{" "}
          <span className="mono">{tildePath(mismatch.found)}</span> — so the
          earlier conversation is not shown.
        </div>
      ) : messages.length === 0 ? (
        <div className="resumed__note">
          The earlier conversation is no longer available.
        </div>
      ) : (
        <SessionTranscript messages={messages} />
      )}

      <div className="resumed__rule">
        <span>Continued here</span>
      </div>
    </div>
  );
}

function ErrorBar({ text, onClose }: { text: string; onClose(): void }) {
  return (
    <div className="banner banner--error">
      <Icon name="alert" size={13} />
      <span className="truncate">{text}</span>
      <div className="spacer" />
      <button className="iconbtn" title="Dismiss" onClick={onClose}>
        <Icon name="close" size={12} />
      </button>
    </div>
  );
}

function chatTitle(c: ChatSession): string {
  return c.title?.trim() || "New chat";
}

function tokenLabel(c: ChatSession): string {
  const total = (c.total_input_tokens ?? 0) + (c.total_output_tokens ?? 0);
  return total ? `${compactNumber(total)} tokens` : "No usage";
}
