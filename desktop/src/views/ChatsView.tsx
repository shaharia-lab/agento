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
import type { Agent, ChatDetail, ChatMessage, ChatSession } from "../lib/types";
import {
  Empty,
  InspGroup,
  InspRow,
  Search,
  Segmented,
  Splitter,
} from "../components/ui";
import { Composer } from "./chat/Composer";
import { Transcript } from "./chat/Transcript";
import { useChatStream } from "./chat/useChatStream";
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

export function ChatsView({ inspectorOpen }: { inspectorOpen: boolean }) {
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
  const [newAgent, setNewAgent] = useState("");
  const [newDir, setNewDir] = useState("");
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

  const send = useCallback(async () => {
    const content = draft.trim();
    if (!content || busy) return;

    let id = selected;
    if (!id) {
      try {
        const created = await api.post<ChatSession>("/chats", {
          agent_slug: newAgent,
          working_directory: newDir.trim(),
          model: "",
          settings_profile_id: "",
        });
        id = created.id;
        setSelected(created.id);
        setDrafting(false);
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
  }, [draft, busy, selected, newAgent, newDir, chats, stream]);

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

            <div className="newchat">
              <span className="selectwrap">
                <select
                  className="select select--sm chatpick"
                  value={newAgent}
                  onChange={(e) => setNewAgent(e.target.value)}
                >
                  <option value="">No agent — direct chat</option>
                  {(agents.data ?? []).map((a) => (
                    <option key={a.slug} value={a.slug}>
                      {a.name || a.slug}
                    </option>
                  ))}
                </select>
                <span className="select__chevron">
                  <Icon name="chevronUD" size={12} />
                </span>
              </span>

              <label className="field field--sm newchat__dir">
                <span className="field__icon">
                  <Icon name="folder" size={12} />
                </span>
                <input
                  value={newDir}
                  onChange={(e) => setNewDir(e.target.value)}
                  placeholder="Working directory (optional)"
                  spellCheck={false}
                />
              </label>
              {agents.error && (
                <span className="newchat__note">
                  Agents unavailable — {agents.error}
                </span>
              )}
            </div>

            <div className="transcript scroll">
              <Empty
                icon="sparkle"
                title="Start the conversation"
                text="Pick an agent and working directory, then send the first message. The chat is created when you send."
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
              placeholder={`Message ${newAgent || "Agento"}…`}
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
            ) : messages.length === 0 && stream.chatId !== selected ? (
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
                onAnswer={(text) => void stream.answer(text)}
                onDecide={(allow) => void stream.decide(allow)}
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
                <div className="pathwell mono selectable">
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
