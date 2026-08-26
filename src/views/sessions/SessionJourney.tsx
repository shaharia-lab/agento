import { useMemo, useState } from "react";
import { api } from "../../lib/api";
import { useResource } from "../../lib/hooks";
import { clockTime, compactNumber, duration, integer } from "../../lib/format";
import { Icon, type IconName } from "../../lib/icons";
import type { JourneyStep, JourneyTurn, SessionJourney as Journey } from "../../lib/types";
import { Empty } from "../../components/ui";
import {
  compaction,
  conversationTokens,
  isDelegation,
  subAgent,
  textContent,
  thinkingText,
  toolCall,
  toolResult,
} from "./journeyData";
import "../../styles/journey.css";

/**
 * The session journey: one run read in order, segmented into turns, with each
 * delegated sub-agent's steps nested under the `Task` call that spawned it.
 *
 * The flat transcript beside it is the better view of *what was said*; this is
 * the view of *what happened* — where the tool calls went, which one failed,
 * what was delegated and how long each stretch took. It is also the only place a
 * turn can be seen, which is what makes `turn_count` and everything Insights
 * derives from it inspectable against the run it describes.
 *
 * Two rendering decisions are load-bearing and were both measured on the web
 * build before it was deleted:
 *
 * - **Nothing auto-expands above 30 turns.** The sessions with the most turns
 *   are also the ones whose turns are largest, and opening even three of them up
 *   front is what users experienced as the page freezing on their most expensive
 *   sessions — which are exactly the ones worth reviewing.
 * - **A turn card carries `content-visibility: auto`.** That lets the browser
 *   skip layout and paint for cards scrolled out of view, with a size hint so
 *   the scrollbar stays put. It is what keeps a 100-turn timeline responsive
 *   without hand-rolled virtualization, which would break in-page search.
 *
 * Text is rendered as plain pre-wrapped text rather than through `Markdown`,
 * deliberately: a timeline holds hundreds of steps where a transcript holds
 * dozens of messages, and this is a technical read of a run rather than a
 * rendering of a conversation.
 */
export function SessionJourney({ sessionId }: { sessionId: string }) {
  const journey = useResource<Journey>(
    (signal) => api.get<Journey>(`/claude-sessions/${sessionId}/journey`, signal),
    [sessionId]
  );

  if (journey.error && !journey.data) {
    return (
      <Empty
        icon="alert"
        title="Couldn't load this journey"
        text={journey.error}
        action={
          <button className="btn" onClick={() => journey.reload()}>
            <Icon name="refresh" size={13} />
            Try again
          </button>
        }
      />
    );
  }
  if (!journey.data) {
    return (
      <div className="sess-loading">
        <span className="sess-spinner" />
        Building the timeline…
      </div>
    );
  }

  return <Timeline journey={journey.data} />;
}

/** Above this many turns nothing auto-expands. See the header. */
const AUTO_EXPAND_TURN_LIMIT = 30;

function Timeline({ journey }: { journey: Journey }) {
  const turns = journey.turns ?? [];
  const delegated = conversationTokens(journey.subagent_usage);
  const total = conversationTokens(journey.usage) + delegated;

  return (
    <>
      <div className="jrn-stats">
        {/* Active time, with the raw span in the tooltip: a resumed session's
            span includes every idle day between sittings, so showing that as
            "duration" reported a 6-hour session as 678h. */}
        <span
          title={`Active time — idle gaps are capped at the threshold in Settings. Raw span ${duration(
            journey.total_duration_ms
          )}.`}
        >
          <Icon name="clock" size={11} />
          {duration(journey.active_duration_ms)}
        </span>
        <span>
          {integer(journey.total_turns)}{" "}
          {journey.total_turns === 1 ? "turn" : "turns"}
        </span>
        {/* The session's real total, matching the sessions list. Delegated work
            is named rather than merged, so the split stays legible. */}
        <span title="Conversation tokens (input + output), main thread and delegated together — the figure the sessions list reports.">
          <Icon name="zap" size={11} />
          {compactNumber(total)} tokens
        </span>
        {journey.subagent_count > 0 && (
          <span
            className="jrn-stats__agent"
            title="Tokens spent by the sub-agents this session delegated to, included in the total beside it."
          >
            <Icon name="users" size={11} />
            {integer(journey.subagent_count)}{" "}
            {journey.subagent_count === 1 ? "sub-agent" : "sub-agents"} ·{" "}
            {compactNumber(delegated)} delegated
          </span>
        )}
      </div>

      <div className="transcript scroll">
        <div className="jrn-turns">
          {turns.length === 0 ? (
            <div className="sess-note">
              This transcript holds no conversation turns.
            </div>
          ) : (
            <>
              {turns.length > AUTO_EXPAND_TURN_LIMIT && (
                <div className="sess-note">
                  {integer(turns.length)} turns, all collapsed. Open the ones you
                  need — each renders its steps only once expanded.
                </div>
              )}
              {turns.map((turn) => (
                <TurnCard
                  key={turn.number}
                  turn={turn}
                  defaultOpen={
                    turns.length <= AUTO_EXPAND_TURN_LIMIT && turn.number <= 3
                  }
                />
              ))}
            </>
          )}
        </div>
      </div>
    </>
  );
}

function TurnCard({
  turn,
  defaultOpen,
}: {
  turn: JourneyTurn;
  defaultOpen: boolean;
}) {
  const [open, setOpen] = useState(defaultOpen);
  const tokens = conversationTokens(turn.usage);

  return (
    <div
      className="jrn-turn"
      // A size hint so the scrollbar stays stable while cards below the fold
      // are skipped. See the module header.
      style={{ contentVisibility: "auto", containIntrinsicSize: "auto 31px" }}
    >
      <button className="jrn-turn__head" onClick={() => setOpen((o) => !o)}>
        <Icon
          name="chevronR"
          size={12}
          className={`chev ${open ? "chev--open" : ""}`}
        />
        <span className="jrn-turn__name">Turn {integer(turn.number)}</span>
        <span className="jrn-turn__time tnum">{clockTime(turn.start_time)}</span>
        <div className="spacer" />
        <span className="jrn-turn__meta tnum">
          {duration(turn.duration_ms)}
          {turn.tool_calls > 0 && ` · ${integer(turn.tool_calls)} tools`}
          {tokens > 0 && ` · ${compactNumber(tokens)} tokens`}
        </span>
      </button>
      {open && (
        <div className="jrn-turn__body">
          {turn.steps.map((step, i) => (
            <StepRow key={`${step.type}-${step.timestamp}-${i}`} step={step} />
          ))}
        </div>
      )}
    </div>
  );
}

/** How a step is labelled and iconed, and which tone it takes. */
interface StepStyle {
  icon: IconName;
  label: string;
  tone: "" | "err" | "ok" | "agent" | "muted";
}

function styleFor(step: JourneyStep): StepStyle {
  switch (step.type) {
    case "user_input":
      return { icon: "users", label: "You", tone: "" };
    case "thinking":
      return { icon: "sparkle", label: "Thinking", tone: "muted" };
    case "text_response":
      return { icon: "chat", label: "Claude", tone: "" };
    case "tool_call":
      return isDelegation(step)
        ? { icon: "users", label: "Delegated", tone: "agent" }
        : { icon: "terminal", label: "Tool", tone: "" };
    case "tool_result": {
      const failed = toolResult(step).isError;
      return failed
        ? { icon: "alert", label: "Failed", tone: "err" }
        : { icon: "check", label: "Result", tone: "ok" };
    }
    case "sub_agent":
      return { icon: "users", label: "Sub-agent", tone: "agent" };
    case "thinking_duration":
      return { icon: "clock", label: "Turn", tone: "muted" };
    case "compaction":
      return { icon: "layers", label: "Compacted", tone: "muted" };
    default:
      return { icon: "info", label: step.type, tone: "muted" };
  }
}

function StepRow({ step, nested = false }: { step: JourneyStep; nested?: boolean }) {
  const style = styleFor(step);
  return (
    <div className={`jrn-step ${nested ? "jrn-step--nested" : ""}`}>
      <span className={`jrn-step__tag jrn-step__tag--${style.tone || "plain"}`}>
        <Icon name={style.icon} size={11} />
        {style.label}
      </span>
      <div className="jrn-step__body">
        <StepContent step={step} />
      </div>
      {!nested && (
        <span className="jrn-step__when tnum">
          {clockTime(step.timestamp)}
          {step.duration_ms ? ` · ${duration(step.duration_ms)}` : ""}
        </span>
      )}
    </div>
  );
}

function StepContent({ step }: { step: JourneyStep }) {
  switch (step.type) {
    case "user_input":
    case "text_response":
      return <TruncatedText text={textContent(step)} />;
    case "thinking":
      return <ThinkingBody step={step} />;
    case "tool_call":
      return <ToolCallBody step={step} />;
    case "tool_result": {
      const result = toolResult(step);
      return (
        <Disclosure
          label={result.isError ? "error output" : "output"}
          body={result.content}
          tone={result.isError ? "err" : ""}
        />
      );
    }
    case "sub_agent":
      return <SubAgentBody step={step} />;
    case "thinking_duration":
      return (
        <span className="jrn-note">
          completed in {duration(step.duration_ms)}
        </span>
      );
    case "compaction": {
      const c = compaction(step);
      return (
        <span className="jrn-note">
          Context compacted{c.trigger ? ` (${c.trigger})` : ""}
          {c.preTokens > 0 &&
            ` · ${compactNumber(c.preTokens)} → ${compactNumber(
              c.postTokens
            )} tokens, ${compactNumber(c.droppedTokens)} dropped`}
        </span>
      );
    }
    default:
      return null;
  }
}

/**
 * How much of a step's text renders before it is cut off behind a button.
 *
 * A pasted file or a long tool-driven answer runs to tens of thousands of
 * characters and a single turn can hold dozens of them; rendering all of it as
 * one text node is what made the largest sessions hang the tab.
 */
const MAX_INLINE_CHARS = 2_000;

function TruncatedText({ text }: { text: string }) {
  const [expanded, setExpanded] = useState(false);
  const long = text.length > MAX_INLINE_CHARS;
  if (!text) return null;
  return (
    <>
      <div className="jrn-text">
        {expanded || !long ? text : `${text.slice(0, MAX_INLINE_CHARS)}…`}
      </div>
      {long && (
        <button className="jrn-more" onClick={() => setExpanded((e) => !e)}>
          {expanded
            ? "Show less"
            : `Show all ${integer(text.length)} characters`}
        </button>
      )}
    </>
  );
}

function ThinkingBody({ step }: { step: JourneyStep }) {
  const [expanded, setExpanded] = useState(false);
  const { preview, full } = thinkingText(step);
  if (!preview && !full) return null;
  return (
    <>
      <div className="jrn-text jrn-text--muted">{expanded ? full : preview}</div>
      {full.length > preview.length && (
        <button className="jrn-more" onClick={() => setExpanded((e) => !e)}>
          {expanded ? "Collapse" : "Expand thinking"}
        </button>
      )}
    </>
  );
}

/**
 * A body behind a Show/Hide button.
 *
 * `body` is a string rather than a thunk: the server already caps a tool result
 * at 2000 characters, so there is no expensive `JSON.stringify` to defer the way
 * the web build had to for a raw tool input.
 */
function Disclosure({
  label,
  body,
  tone,
}: {
  label: string;
  body: string;
  tone: "" | "err";
}) {
  const [open, setOpen] = useState(false);
  if (!body) return <span className="jrn-note">no output</span>;
  return (
    <>
      <button className="jrn-more" onClick={() => setOpen((o) => !o)}>
        {open ? "Hide" : "Show"} {label}
      </button>
      {open && (
        <pre className={`jrn-pre ${tone === "err" ? "jrn-pre--err" : ""}`}>
          {body}
        </pre>
      )}
    </>
  );
}

function ToolCallBody({ step }: { step: JourneyStep }) {
  const call = toolCall(step);
  return (
    <>
      <div className="jrn-call">
        <span className="jrn-call__name">{call.toolName || "tool"}</span>
        {call.agentType && (
          <span className="badge badge--purple">{call.agentType}</span>
        )}
        {call.description && (
          <span className="truncate">{call.description}</span>
        )}
        {call.agentUsage && (
          <span className="jrn-call__usage tnum">
            {compactNumber(conversationTokens(call.agentUsage))} tokens
          </span>
        )}
      </div>
      {call.input !== undefined && call.input !== null && (
        <Disclosure
          label="input"
          body={JSON.stringify(call.input, null, 2)}
          tone=""
        />
      )}
      <NestedSteps steps={step.steps} />
    </>
  );
}

function SubAgentBody({ step }: { step: JourneyStep }) {
  const agent = subAgent(step);
  return (
    <>
      <div className="jrn-call">
        {agent.agentType && (
          <span className="badge badge--purple">{agent.agentType}</span>
        )}
        <span className="truncate">
          {agent.description || agent.agentId || "sub-agent"}
        </span>
        {agent.usage && (
          <span className="jrn-call__usage tnum">
            {compactNumber(conversationTokens(agent.usage))} tokens
          </span>
        )}
      </div>
      {/* This step exists because the `Task` call that spawned it is not in the
          rendered transcript — compacted away, or its sidecar carried no
          toolUseId. Saying so is what stops it reading as a stray row. */}
      <span className="jrn-note">
        delegated work whose originating call is not in this transcript
      </span>
      <NestedSteps steps={step.steps} />
    </>
  );
}

/** One delegated agent's own steps, collapsed behind an "N steps" line. */
function NestedSteps({ steps }: { steps: JourneyStep[] | null | undefined }) {
  const [open, setOpen] = useState(false);
  const nested = useMemo(() => steps ?? [], [steps]);
  if (nested.length === 0) return null;
  return (
    <>
      <button className="jrn-more" onClick={() => setOpen((o) => !o)}>
        <Icon
          name="chevronR"
          size={11}
          className={`chev ${open ? "chev--open" : ""}`}
        />
        {integer(nested.length)} delegated{" "}
        {nested.length === 1 ? "step" : "steps"}
      </button>
      {open && (
        <div className="jrn-nested">
          {nested.map((s, i) => (
            <StepRow key={`${s.type}-${s.timestamp}-${i}`} step={s} nested />
          ))}
        </div>
      )}
    </>
  );
}
