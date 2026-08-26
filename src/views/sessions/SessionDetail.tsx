import { useMemo, useState } from "react";
import { api } from "../../lib/api";
import { useResource } from "../../lib/hooks";
import { compactNumber, dateTime, integer, tildePath, usd } from "../../lib/format";
import { Icon } from "../../lib/icons";
import type {
  ClaudeMessage,
  ClaudeSessionDetail,
  ClaudeSessionSummary,
  ClaudeSubagent,
  ClaudeTodo,
} from "../../lib/types";
import { Empty, Segmented } from "../../components/ui";
import { Thinking, ToolCall } from "../chat/Transcript";
import type { ToolState } from "../chat/useChatStream";
import { Markdown } from "../chat/Markdown";
import { SessionJourney } from "./SessionJourney";

/**
 * Read-only transcript of one indexed Claude Code session, rendered with the
 * same primitives the live chat transcript uses. The row the list handed over
 * paints the header immediately; the full transcript loads behind it.
 */
export function SessionDetail({
  session,
  onBack,
  onContinue,
  continuing,
  continueError,
}: {
  session: ClaudeSessionSummary;
  onBack(): void;
  /**
   * Shared with the list inspector's own button rather than duplicated: both
   * surfaces must create the chat *and* navigate, and two copies is where one
   * of them stops doing the second half (#485).
   */
  onContinue(s: ClaudeSessionSummary): void;
  continuing: boolean;
  continueError?: string;
}) {
  // Which of the two readings of this session fills the pane. The header, the
  // meta row and the back button are shared, so this is a tab rather than a
  // second screen — and `Segmented` is the repo's tab primitive.
  //
  // The journey mounts only while it is selected, so a session nobody opens the
  // timeline for never pays for the extra whole-transcript read the endpoint
  // does; switching back and forth re-fetches, which is the same trade every
  // other view in the app makes.
  const [tab, setTab] = useState<Tab>("transcript");

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
  //
  // A carrier does now arrive with `blocks`, and it is still not a turn: its
  // results are read out below and rendered against the call each answers, so
  // "has something to show" is any block that is *not* a tool_result.
  const messages = useMemo(
    () =>
      (detail.data?.messages ?? []).filter((m) => {
        if (m.is_sidechain) return false;
        if (m.type !== "user") return true;
        const text = m.content?.trim() ?? "";
        if (!text && !m.blocks?.some((b) => b.type !== "tool_result")) {
          return false;
        }
        return !INJECTED_MARKERS.some((w) => text.startsWith(w));
      }),
    [detail.data]
  );

  // A tool call and its result are two messages apart in a transcript — the
  // assistant's `tool_use`, then the user-role carrier answering it — so the
  // results are collected once, keyed on the id both blocks publish, and read
  // back where the call renders. This walks the *unfiltered* list on purpose:
  // the carriers holding the results are exactly what the filter above drops.
  const tools = useMemo(() => {
    const byId: Record<string, ToolState> = {};
    for (const m of detail.data?.messages ?? []) {
      for (const b of m.blocks ?? []) {
        if (b.type !== "tool_result" || !b.id) continue;
        byId[b.id] = { result: b.text ?? "", isError: b.is_error ?? false };
      }
    }
    return byId;
  }, [detail.data]);

  const todos = detail.data?.todos ?? [];
  const subagents = detail.data?.subagents ?? [];
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
        <Segmented<Tab> value={tab} options={TABS} onChange={setTab} />
        <div className="toolbar__sep" />
        <span className="toolbar__sub tnum">
          {integer(session.message_count)} msgs ·{" "}
          {usd(
            (session.cost?.total_usd ?? 0) +
              (session.subagent_cost?.total_usd ?? 0)
          )}
        </span>
        <div className="toolbar__sep" />
        <button
          className="btn"
          disabled={continuing}
          onClick={() => onContinue(session)}
        >
          <Icon name="play" size={13} />
          {continuing ? "Starting…" : "Continue in chat"}
        </button>
      </div>

      {/* A success leaves this view entirely, so this only ever renders a
          failure — and it renders in the toolbar's own band rather than in a
          pane the user would have to scroll. */}
      {continueError && (
        <div className="sess-detail__error">{continueError}</div>
      )}

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

      {tab === "journey" ? (
        <SessionJourney sessionId={session.session_id} />
      ) : detail.error && !detail.data ? (
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
            {subagents.length > 0 && <SubagentList subagents={subagents} />}
            {messages.length === 0 ? (
              <div className="sess-note">
                This transcript holds no conversation turns.
              </div>
            ) : (
              messages.map((m, i) => (
                <Message key={m.uuid || i} msg={m} tools={tools} />
              ))
            )}
          </div>
        </div>
      )}
    </>
  );
}

/** The two readings of one session; see the `tab` state. */
type Tab = "transcript" | "journey";

const TABS: { value: Tab; label: string }[] = [
  { value: "transcript", label: "Transcript" },
  { value: "journey", label: "Journey" },
];

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

/**
 * The byline every assistant message carries.
 *
 * A constant, and deliberately not the session's `agent_name`: Claude Code's
 * `agent-name` event is the session's own name — what `/rename` sets — in every
 * version that has written one, so using it here bylined all 104 messages of a
 * renamed session with its 100-character title. `lib/sessionAgent.ts` carries
 * the evidence and the rest of the rule.
 */
const ASSISTANT_LABEL = "Claude";

function Message({
  msg,
  tools,
}: {
  msg: ClaudeMessage;
  tools: Record<string, ToolState>;
}) {
  const isUser = msg.type === "user";
  // Blocks replace `content` as the body, so a message whose only blocks are
  // tool_results must fall back to it rather than rendering nothing: those are
  // shown against the call each answers, not here. No transcript in the local
  // corpus writes text beside a tool_result (0 of 10,568 carriers), but the
  // alternative to this line is a silently empty bubble if one ever does.
  const blocks = msg.blocks?.some((b) => b.type !== "tool_result")
    ? msg.blocks
    : null;

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
          <span className="msg__author">
            {isUser ? "You" : ASSISTANT_LABEL}
          </span>
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
                // The result of this call, if the transcript carried one.
                // `undefined` now means genuinely unanswered — an interrupted
                // session — rather than "never looked".
                state={b.id ? tools[b.id] : undefined}
                live={false}
              />
            ) : b.type === "tool_result" ? (
              // Rendered inside the call it answers, never on its own. Only a
              // carrier holds one, and a carrier is not shown.
              null
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

/**
 * The delegations this session spawned, as one collapsed block.
 *
 * The toolbar's figure is `cost + subagent_cost`, so without this the page
 * charges for work it never shows. It is a *summary* and not a transcript:
 * a sub-agent's own messages are sidechain events, and interleaving them into
 * the parent's flat list reads as nonsense. Nesting them under the `Task` call
 * that spawned them is the Journey tab's job, and since #479 that tab exists —
 * this stays as the at-a-glance count beside the flat read.
 */
function SubagentList({ subagents }: { subagents: ClaudeSubagent[] }) {
  const [open, setOpen] = useState(false);
  const tokens = subagents.reduce(
    (n, s) => n + s.usage.input_tokens + s.usage.output_tokens,
    0
  );

  return (
    <div className="toolcall">
      <button className="toolcall__head" onClick={() => setOpen((o) => !o)}>
        <Icon
          name="chevronR"
          size={12}
          className={`chev ${open ? "chev--open" : ""}`}
        />
        <Icon name="users" size={13} />
        <span className="toolcall__name">Sub-agents</span>
        <span className="truncate">
          {integer(subagents.length)} delegated ·{" "}
          {compactNumber(tokens)} tokens
        </span>
      </button>
      {open && (
        <div className="toolcall__body sess-subagents">
          {subagents.map((s) => (
            <div key={s.agent_id} className="sess-subagent">
              <div className="sess-subagent__head">
                <span className="sess-subagent__type">
                  {s.agent_type || "sub-agent"}
                </span>
                <span className="sess-subagent__meta tnum">
                  {integer(s.message_count)} msgs ·{" "}
                  {compactNumber(
                    s.usage.input_tokens + s.usage.output_tokens
                  )}{" "}
                  tokens
                  {s.model ? ` · ${s.model}` : ""}
                </span>
              </div>
              {s.description && (
                <div className="sess-subagent__desc">{s.description}</div>
              )}
            </div>
          ))}
        </div>
      )}
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
