import { useMemo } from "react";
import { api } from "../../lib/api";
import { useResource } from "../../lib/hooks";
import { dateTime, integer, tildePath, usd } from "../../lib/format";
import { Icon } from "../../lib/icons";
import type {
  ClaudeMessage,
  ClaudeSessionDetail,
  ClaudeSessionSummary,
  ClaudeTodo,
} from "../../lib/types";
import { Empty } from "../../components/ui";
import { Thinking, ToolCall } from "../chat/Transcript";
import { Markdown } from "../chat/Markdown";

/**
 * Read-only transcript of one indexed Claude Code session, rendered with the
 * same primitives the live chat transcript uses. The row the list handed over
 * paints the header immediately; the full transcript loads behind it.
 */
export function SessionDetail({
  session,
  onBack,
}: {
  session: ClaudeSessionSummary;
  onBack(): void;
}) {
  const detail = useResource<ClaudeSessionDetail>(
    (signal) =>
      api.get<ClaudeSessionDetail>(
        `/claude-sessions/${session.session_id}`,
        signal
      ),
    [session.session_id]
  );

  // Sidechain events are a sub-agent's own transcript leaking into the
  // parent's, a user event with nothing to show is a tool_result carrier, and
  // content opening with one of Claude Code's own injection wrappers is the
  // harness talking, not the user — the scanner's turn count excludes all
  // three, so showing them here would render turns the header does not count.
  const messages = useMemo(
    () =>
      (detail.data?.messages ?? []).filter((m) => {
        if (m.is_sidechain) return false;
        if (m.type !== "user") return true;
        const text = m.content?.trim() ?? "";
        if (!text && !m.blocks?.length) return false;
        return !INJECTED_MARKERS.some((w) => text.startsWith(w));
      }),
    [detail.data]
  );
  const todos = detail.data?.todos ?? [];
  const title = detail.data?.display_title || session.display_title;

  return (
    <>
      <div className="toolbar">
        <button className="btn btn--ghost" onClick={onBack}>
          <Icon name="chevronR" size={13} style={{ transform: "rotate(180deg)" }} />
          Sessions
        </button>
        <div className="toolbar__sep" />
        <div className="toolbar__title truncate" title={title}>
          {title || "Untitled session"}
        </div>
        <div className="spacer" />
        <span className="toolbar__sub tnum">
          {integer(session.message_count)} msgs ·{" "}
          {usd(
            (session.cost?.total_usd ?? 0) +
              (session.subagent_cost?.total_usd ?? 0)
          )}
        </span>
      </div>

      <div className="sess-detail__meta">
        <span className="mono" title={session.project_path}>
          {tildePath(session.project_path)}
        </span>
        {session.git_branch && (
          <span>
            <Icon name="branch" size={11} /> {session.git_branch}
          </span>
        )}
        {session.model && <span>{session.model}</span>}
        <span>{dateTime(session.start_time)}</span>
      </div>

      {detail.error && !detail.data ? (
        <Empty
          icon="alert"
          title="Couldn't load this session"
          text={detail.error}
          action={
            <button className="btn" onClick={() => detail.reload()}>
              <Icon name="refresh" size={13} />
              Try again
            </button>
          }
        />
      ) : !detail.data ? (
        <div className="sess-loading">
          <span className="sess-spinner" />
          Reading transcript…
        </div>
      ) : (
        <div className="transcript scroll">
          <div className="transcript__inner">
            {todos.length > 0 && <TodoList todos={todos} />}
            {messages.length === 0 ? (
              <div className="sess-note">
                This transcript holds no conversation turns.
              </div>
            ) : (
              messages.map((m, i) => (
                <Message key={m.uuid || i} msg={m} agent={agentLabel(detail.data)} />
              ))
            )}
          </div>
        </div>
      )}
    </>
  );
}

/**
 * The harness's injection wrappers — internal/claudesessions'
 * injectedTurnMarkers, plus the skill-invocation preamble.
 */
const INJECTED_MARKERS = [
  "<task-notification>",
  "<command-message>",
  "<command-name>",
  "<local-command-caveat>",
  "<local-command-stdout>",
  "<system-reminder>",
  "Base directory for this skill:",
];

function agentLabel(d: ClaudeSessionDetail | null | undefined): string {
  return d?.agent_name || "Claude";
}

function Message({ msg, agent }: { msg: ClaudeMessage; agent: string }) {
  const isUser = msg.type === "user";
  const blocks = msg.blocks?.length ? msg.blocks : null;

  return (
    <div className={`msg ${isUser ? "msg--user" : ""}`}>
      <div
        className={`avatar ${isUser ? "" : "avatar--purple"}`}
        style={{ width: 28, height: 28 }}
      >
        {isUser ? <Icon name="users" size={13} /> : <Icon name="sparkle" size={13} />}
      </div>
      <div className="msg__body">
        <div className="msg__head">
          <span className="msg__author">{isUser ? "You" : agent}</span>
          <span className="msg__time">{dateTime(msg.timestamp)}</span>
        </div>
        {blocks ? (
          blocks.map((b, i) =>
            b.type === "thinking" ? (
              // Redacted thinking is an empty block plus a signature — there
              // is nothing to open, so no box is drawn.
              b.text ? <Thinking key={i} text={b.text} /> : null
            ) : b.type === "tool_use" ? (
              <ToolCall
                key={b.id ?? i}
                name={b.name ?? "tool"}
                input={b.input}
                state={undefined}
                live={false}
              />
            ) : (
              <Markdown key={i} text={b.text ?? ""} />
            )
          )
        ) : (
          <Markdown text={msg.content ?? ""} />
        )}
      </div>
    </div>
  );
}

function TodoList({ todos }: { todos: ClaudeTodo[] }) {
  const done = todos.filter((t) => t.status === "completed").length;
  return (
    <div className="toolcall">
      <div className="toolcall__head" style={{ pointerEvents: "none" }}>
        <Icon name="task" size={13} />
        <span className="toolcall__name">Todos</span>
        <span className="truncate">
          {done}/{todos.length} completed
        </span>
      </div>
      <div className="toolcall__body sess-todos">
        {todos.map((t, i) => (
          <div key={i} className={`sess-todo sess-todo--${t.status}`}>
            <Icon
              name={t.status === "completed" ? "check" : "clock"}
              size={11}
            />
            <span>{t.content}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
