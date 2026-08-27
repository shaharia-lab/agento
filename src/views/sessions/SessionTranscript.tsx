import { useMemo, type ReactNode } from "react";
import { dateTime } from "../../lib/format";
import { Icon } from "../../lib/icons";
import type { ClaudeMessage } from "../../lib/types";
import { Thinking, ToolCall } from "../chat/Transcript";
import type { ToolState } from "../chat/useChatStream";
import { Markdown } from "../chat/Markdown";

/**
 * A Claude Code transcript, read-only, rendered with the same primitives the
 * live chat transcript uses.
 *
 * Lifted out of `SessionDetail` by #490, which gave it a second caller: a chat
 * created by "Continue in chat" renders the conversation it resumes above its
 * own messages, and forking this would fork the tool-result and sub-agent
 * handling #483 had only just landed.
 *
 * **`messages` must be the unfiltered list**, exactly as the endpoint returned
 * it (or a *prefix* of it — see `continued_from_message_count`). The filtering
 * below and the tool-result lookup disagree on purpose: a tool result lives on a
 * carrier message that the filter drops, so the results are collected from the
 * whole list and only the display list is narrowed.
 */
export function SessionTranscript({
  messages,
  empty,
}: {
  messages: ClaudeMessage[];
  /** Rendered when nothing in `messages` is a conversation turn. */
  empty?: ReactNode;
}) {
  const visible = useMemo(() => visibleMessages(messages), [messages]);
  const tools = useMemo(() => toolStates(messages), [messages]);

  if (visible.length === 0) return <>{empty ?? null}</>;

  return (
    <>
      {visible.map((m, i) => (
        <SessionMessage key={m.uuid || i} msg={m} tools={tools} />
      ))}
    </>
  );
}

/**
 * The turns a reader should see.
 *
 * Sidechain events are a sub-agent's own transcript leaking into the parent's, a
 * user event with nothing to show is a tool_result carrier, and content opening
 * with one of Claude Code's own injection wrappers is the harness talking, not
 * the user — the scanner's turn count excludes all three, so showing them here
 * would render turns the header does not count.
 *
 * A carrier does arrive with `blocks`, and it is still not a turn: its results
 * are read out below and rendered against the call each answers, so "has
 * something to show" is any block that is *not* a tool_result.
 */
export function visibleMessages(all: ClaudeMessage[]): ClaudeMessage[] {
  return all.filter((m) => {
    if (m.is_sidechain) return false;
    if (m.type !== "user") return true;
    const text = m.content?.trim() ?? "";
    if (!text && !m.blocks?.some((b) => b.type !== "tool_result")) return false;
    return !INJECTED_MARKERS.some((w) => text.startsWith(w));
  });
}

/**
 * Every tool result in the transcript, keyed on the id its call publishes.
 *
 * A tool call and its result are two messages apart — the assistant's
 * `tool_use`, then the user-role carrier answering it — so they are collected
 * once and read back where the call renders. This walks the **unfiltered** list
 * on purpose: the carriers holding the results are exactly what
 * [`visibleMessages`] drops.
 */
export function toolStates(all: ClaudeMessage[]): Record<string, ToolState> {
  const byId: Record<string, ToolState> = {};
  for (const m of all) {
    for (const b of m.blocks ?? []) {
      if (b.type !== "tool_result" || !b.id) continue;
      byId[b.id] = { result: b.text ?? "", isError: b.is_error ?? false };
    }
  }
  return byId;
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

function SessionMessage({
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
          <span className="msg__author">{isUser ? "You" : ASSISTANT_LABEL}</span>
          <span className="msg__time">{dateTime(msg.timestamp)}</span>
        </div>
        {blocks ? (
          blocks.map((b, i) =>
            b.type === "thinking" ? (
              // Redacted thinking is an empty block plus a signature — there is
              // nothing to open, so no box is drawn.
              b.text ? <Thinking key={i} text={b.text} /> : null
            ) : b.type === "tool_use" ? (
              <ToolCall
                key={b.id ?? i}
                name={b.name ?? "tool"}
                input={b.input}
                // The result of this call, if the transcript carried one.
                // `undefined` means genuinely unanswered — an interrupted
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
