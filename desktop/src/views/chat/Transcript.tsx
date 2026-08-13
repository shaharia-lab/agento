import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Icon } from "../../lib/icons";
import { clockTime, initials, type Tone } from "../../lib/format";
import type { ChatMessage, MessageBlock } from "../../lib/types";
import { prettyJson, summarizeInput, type QuestionItem } from "./sse";
import type { Live, Prompt, ToolState } from "./useChatStream";

export function Transcript({
  chatId,
  messages,
  agent,
  tone,
  live,
  tools,
  prompt,
  streaming,
  onAnswer,
  onDecide,
}: {
  chatId: string;
  messages: ChatMessage[];
  agent: string;
  tone: Tone;
  live: Live | null;
  tools: Record<string, ToolState>;
  prompt: Prompt | null;
  streaming: boolean;
  onAnswer(text: string): void;
  onDecide(allow: boolean): void;
}) {
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);

  // Jump to the newest turn when the conversation changes, then follow new
  // content only while the reader is already at the bottom.
  useEffect(() => {
    stick.current = true;
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [chatId]);

  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  });

  const onScroll = () => {
    const el = scroller.current;
    if (!el) return;
    stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
  };

  return (
    <div className="transcript scroll" ref={scroller} onScroll={onScroll}>
      <div className="transcript__inner">
        {messages.map((m, i) => (
          <Message
            key={`${m.timestamp}-${i}`}
            msg={m}
            agent={agent}
            tone={tone}
            tools={tools}
          />
        ))}

        {live && <LiveTurn live={live} agent={agent} tone={tone} tools={tools} />}

        {prompt?.kind === "question" && (
          <QuestionPrompt questions={prompt.questions} onAnswer={onAnswer} />
        )}
        {prompt?.kind === "permission" && (
          <PermissionPrompt
            toolName={prompt.request.toolName}
            input={prompt.request.input}
            onDecide={onDecide}
          />
        )}

        {streaming && !prompt && <Running live={live} tools={tools} />}
      </div>
    </div>
  );
}

/* --- Stored turns -------------------------------------------------------- */

function Message({
  msg,
  agent,
  tone,
  tools,
}: {
  msg: ChatMessage;
  agent: string;
  tone: Tone;
  tools: Record<string, ToolState>;
}) {
  const isUser = msg.role === "user";
  const blocks = msg.blocks?.length ? msg.blocks : null;

  return (
    <div className={`msg ${isUser ? "msg--user" : ""}`}>
      <div
        className={`avatar ${isUser ? "" : `avatar--${tone}`}`}
        style={{ width: 28, height: 28 }}
      >
        {isUser ? <Icon name="users" size={13} /> : initials(agent)}
      </div>
      <div className="msg__body">
        <div className="msg__head">
          <span className="msg__author">{isUser ? "You" : agent}</span>
          <span className="msg__time">{clockTime(msg.timestamp)}</span>
        </div>
        {blocks ? (
          <Blocks blocks={blocks} tools={tools} live={false} />
        ) : (
          <RichText text={msg.content} />
        )}
      </div>
    </div>
  );
}

function Blocks({
  blocks,
  tools,
  live,
}: {
  blocks: MessageBlock[];
  tools: Record<string, ToolState>;
  /** Only a turn still in flight can have a tool that has not returned yet. */
  live: boolean;
}) {
  return (
    <>
      {blocks.map((b, i) => {
        if (b.type === "thinking") {
          return <Thinking key={i} text={b.text ?? ""} />;
        }
        if (b.type === "tool_use") {
          return (
            <ToolCall
              key={b.id ?? i}
              name={b.name ?? "tool"}
              input={b.input}
              state={b.id ? tools[b.id] : undefined}
              live={live}
            />
          );
        }
        return <RichText key={i} text={b.text ?? ""} />;
      })}
    </>
  );
}

/* --- The turn being streamed --------------------------------------------- */

function LiveTurn({
  live,
  agent,
  tone,
  tools,
}: {
  live: Live;
  agent: string;
  tone: Tone;
  tools: Record<string, ToolState>;
}) {
  if (!live.blocks.length && !live.text && !live.thinking) return null;

  return (
    <div className="msg">
      <div className={`avatar avatar--${tone}`} style={{ width: 28, height: 28 }}>
        {initials(agent)}
      </div>
      <div className="msg__body">
        <div className="msg__head">
          <span className="msg__author">{agent}</span>
          <span className="msg__time">now</span>
        </div>
        <Blocks blocks={live.blocks} tools={tools} live />
        {live.thinking && <Thinking text={live.thinking} defaultOpen />}
        {live.text && <RichText text={live.text} caret />}
      </div>
    </div>
  );
}

function Running({
  live,
  tools,
}: {
  live: Live | null;
  tools: Record<string, ToolState>;
}) {
  return (
    <div className="msg">
      <div style={{ width: 28, flex: "none" }} />
      <div className="msg__body">
        <div className="runline">
          <span className="dot dot--green dot--pulse" />
          {statusOf(live, tools)}
        </div>
      </div>
    </div>
  );
}

function statusOf(live: Live | null, tools: Record<string, ToolState>): string {
  if (!live) return "Starting…";
  for (let i = live.blocks.length - 1; i >= 0; i--) {
    const b = live.blocks[i];
    if (b.type !== "tool_use") continue;
    const state = b.id ? tools[b.id] : undefined;
    if (state?.result !== undefined) break;
    return state?.progress
      ? `${b.name ?? "Tool"} — ${state.progress}`
      : `Running ${b.name ?? "tool"}…`;
  }
  if (live.thinking && !live.text) return "Thinking…";
  if (live.text) return "Writing…";
  return "Working…";
}

/* --- Blocks -------------------------------------------------------------- */

function Thinking({ text, defaultOpen }: { text: string; defaultOpen?: boolean }) {
  const [open, setOpen] = useState(defaultOpen ?? false);
  return (
    <div className="toolcall">
      <button className="toolcall__head" onClick={() => setOpen((o) => !o)}>
        <Icon
          name="chevronR"
          size={12}
          className={`chev ${open ? "chev--open" : ""}`}
        />
        <Icon name="sparkle" size={13} />
        <span className="toolcall__name">Thinking</span>
        <span className="truncate">{open ? "" : firstLine(text)}</span>
      </button>
      {open && <div className="toolcall__body">{text}</div>}
    </div>
  );
}

function ToolCall({
  name,
  input,
  state,
  live,
}: {
  name: string;
  input: unknown;
  state: ToolState | undefined;
  live: boolean;
}) {
  const [open, setOpen] = useState(false);
  const done = state?.result !== undefined;

  return (
    <div className="toolcall">
      <button className="toolcall__head" onClick={() => setOpen((o) => !o)}>
        <Icon
          name="chevronR"
          size={12}
          className={`chev ${open ? "chev--open" : ""}`}
        />
        <Icon name="terminal" size={13} />
        <span className="toolcall__name">{name}</span>
        <span className="truncate">{summarizeInput(input)}</span>
        <div className="spacer" />
        {state?.isError ? (
          <Icon name="alert" size={13} style={{ color: "var(--red)" }} />
        ) : done ? (
          <Icon name="check" size={13} style={{ color: "var(--green)" }} />
        ) : live ? (
          <span className="badge badge--amber">
            <span className="dot dot--amber dot--pulse" />
            {state?.progress ?? "Working"}
          </span>
        ) : null}
      </button>
      {open && (
        <div className="toolcall__body">
          {prettyJson(input)}
          {state?.result !== undefined && (
            <>
              <div className="toolcall__sep">
                {state.isError ? "Error" : "Result"}
              </div>
              {clip(state.result)}
            </>
          )}
        </div>
      )}
    </div>
  );
}

/* --- Inline prompts ------------------------------------------------------ */

function QuestionPrompt({
  questions,
  onAnswer,
}: {
  questions: QuestionItem[];
  onAnswer(text: string): void;
}) {
  // One selection set per question index; the answer the API takes is free
  // text, so options are a shortcut for composing it rather than the payload.
  const [picked, setPicked] = useState<Record<number, string[]>>({});
  const [free, setFree] = useState("");

  const toggle = (qi: number, label: string, multi: boolean) =>
    setPicked((prev) => {
      const cur = prev[qi] ?? [];
      if (cur.includes(label)) {
        return { ...prev, [qi]: cur.filter((l) => l !== label) };
      }
      return { ...prev, [qi]: multi ? [...cur, label] : [label] };
    });

  const composed = compose(questions, picked, free);

  return (
    <div className="msg">
      <div style={{ width: 28, flex: "none" }} />
      <div className="msg__body">
        <div className="prompt">
          <div className="prompt__head">
            <Icon name="bulb" size={13} />
            <span className="prompt__title">The agent needs your input</span>
          </div>

          {questions.map((q, qi) => (
            <div key={qi} className="prompt__q">
              {q.header && <div className="prompt__label">{q.header}</div>}
              <div className="prompt__text">{q.question}</div>
              {q.options.length > 0 && (
                <div className="prompt__opts">
                  {q.options.map((o) => {
                    const on = (picked[qi] ?? []).includes(o.label);
                    return (
                      <button
                        key={o.label}
                        className={`prompt__opt ${on ? "prompt__opt--on" : ""}`}
                        title={o.description}
                        onClick={() => toggle(qi, o.label, q.multiSelect)}
                      >
                        {on && <Icon name="check" size={11} strokeWidth={2.2} />}
                        {o.label}
                      </button>
                    );
                  })}
                </div>
              )}
            </div>
          ))}

          <textarea
            className="prompt__input"
            rows={2}
            value={free}
            placeholder={
              questions.length ? "Add anything else…" : "Type your answer…"
            }
            onChange={(e) => setFree(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && composed) {
                onAnswer(composed);
              }
            }}
          />

          <div className="prompt__actions">
            <button
              className="btn btn--primary"
              disabled={!composed}
              onClick={() => onAnswer(composed)}
            >
              Send answer
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

function compose(
  questions: QuestionItem[],
  picked: Record<number, string[]>,
  free: string
): string {
  const lines: string[] = [];
  questions.forEach((q, i) => {
    const sel = picked[i];
    if (!sel?.length) return;
    lines.push(`${q.header || q.question || `Q${i + 1}`}: ${sel.join(", ")}`);
  });
  if (free.trim()) lines.push(free.trim());
  return lines.join("\n");
}

function PermissionPrompt({
  toolName,
  input,
  onDecide,
}: {
  toolName: string;
  input: unknown;
  onDecide(allow: boolean): void;
}) {
  return (
    <div className="msg">
      <div style={{ width: 28, flex: "none" }} />
      <div className="msg__body">
        <div className="prompt prompt--permission">
          <div className="prompt__head">
            <Icon name="shield" size={13} />
            <span className="prompt__title">
              Allow <span className="mono">{toolName}</span>?
            </span>
          </div>
          <div className="prompt__pre mono selectable">{clip(prettyJson(input))}</div>
          <div className="prompt__actions">
            <button className="btn" onClick={() => onDecide(false)}>
              Deny
            </button>
            <button className="btn btn--primary" onClick={() => onDecide(true)}>
              Allow
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

/* --- Text ---------------------------------------------------------------- */

/** Paragraphs, plus fenced code lifted out so long output stays scannable. */
function RichText({ text, caret }: { text: string; caret?: boolean }) {
  if (!text) return null;
  const parts = text.split(/```/);
  return (
    <div className="msg__text">
      {parts.map((part, i) =>
        i % 2 === 1 ? (
          <pre key={i} className="codeblock selectable">
            {part.replace(/^[^\n]*\n/, "").replace(/\n$/, "")}
          </pre>
        ) : (
          part
            .split(/\n{2,}/)
            .filter((p) => p.trim())
            .map((p, j) => <p key={`${i}-${j}`}>{p}</p>)
        )
      )}
      {caret && <span className="caret" />}
    </div>
  );
}

function firstLine(text: string): string {
  const line = text.trim().split("\n")[0] ?? "";
  return line.length > 90 ? `${line.slice(0, 90)}…` : line;
}

/** Tool output can be a whole file; the disclosure body is not a pager. */
function clip(text: string, max = 4000): string {
  return text.length > max ? `${text.slice(0, max)}\n…truncated` : text;
}
