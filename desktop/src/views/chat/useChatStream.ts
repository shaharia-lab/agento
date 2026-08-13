/* ============================================================================
   The turn state machine.

   One chat streams at a time. The hook owns everything that only exists while
   a turn is in flight — deltas, tool state, the pending prompt — and hands the
   finished assistant message back to the view when the stream closes.
   ========================================================================== */

import { useCallback, useEffect, useRef, useState } from "react";
import { api, streamChatMessage } from "../../lib/api";
import { describeError } from "../../lib/hooks";
import type { ChatMessage, MessageBlock } from "../../lib/types";
import {
  parseData,
  readBlocks,
  readDelta,
  readError,
  readPermission,
  readQuestions,
  readResult,
  readSystemInit,
  readToolProgress,
  readToolResults,
  type PermissionRequest,
  type QuestionItem,
  type SystemInit,
  type TurnResult,
} from "./sse";

export interface ToolState {
  progress?: string;
  result?: string;
  isError?: boolean;
}

export type Prompt =
  | { kind: "question"; questions: QuestionItem[] }
  | { kind: "permission"; request: PermissionRequest };

export interface Live {
  /** Blocks confirmed by an `assistant` event. */
  blocks: MessageBlock[];
  /** Deltas for the block currently being produced. */
  text: string;
  thinking: string;
}

const EMPTY: Live = { blocks: [], text: "", thinking: "" };

export interface ChatStream {
  /** The chat this turn belongs to, or null when nothing is in flight. */
  chatId: string | null;
  live: Live | null;
  prompt: Prompt | null;
  tools: Record<string, ToolState>;
  result: TurnResult | null;
  system: SystemInit | null;
  error: string | undefined;
  stopping: boolean;
  start(chatId: string, content: string): void;
  stop(): void;
  answer(text: string): Promise<void>;
  decide(allow: boolean): Promise<void>;
  reset(): void;
  dismissError(): void;
}

export function useChatStream(
  onTurnEnd: (chatId: string, message: ChatMessage | null) => void
): ChatStream {
  const [chatId, setChatId] = useState<string | null>(null);
  const [live, setLive] = useState<Live | null>(null);
  const [prompt, setPrompt] = useState<Prompt | null>(null);
  const [tools, setTools] = useState<Record<string, ToolState>>({});
  const [result, setResult] = useState<TurnResult | null>(null);
  const [system, setSystem] = useState<SystemInit | null>(null);
  const [error, setError] = useState<string>();
  const [stopping, setStopping] = useState(false);

  const abortRef = useRef<(() => void) | null>(null);
  const liveRef = useRef<Live | null>(null);
  const resultRef = useRef<TurnResult | null>(null);
  const endedRef = useRef(false);
  const onEndRef = useRef(onTurnEnd);
  onEndRef.current = onTurnEnd;

  useEffect(() => () => abortRef.current?.(), []);

  const update = useCallback((fn: (l: Live) => Live) => {
    const next = fn(liveRef.current ?? EMPTY);
    liveRef.current = next;
    setLive(next);
  }, []);

  const start = useCallback(
    (id: string, content: string) => {
      if (abortRef.current) return;

      liveRef.current = EMPTY;
      resultRef.current = null;
      endedRef.current = false;
      setLive(EMPTY);
      setResult(null);
      setPrompt(null);
      setError(undefined);
      setStopping(false);
      setChatId(id);

      const finish = () => {
        if (endedRef.current) return;
        endedRef.current = true;
        abortRef.current = null;
        onEndRef.current(id, composeMessage(liveRef.current, resultRef.current));
        liveRef.current = null;
        setLive(null);
        setPrompt(null);
        setStopping(false);
        setChatId(null);
      };

      abortRef.current = streamChatMessage(id, content, {
        onEvent: (msg) => {
          const payload = parseData(msg.data);
          switch (msg.event) {
            case "stream_event": {
              const delta = readDelta(payload);
              if (!delta) return;
              update((l) => ({
                ...l,
                text: delta.text ? l.text + delta.text : l.text,
                thinking: delta.thinking
                  ? l.thinking + delta.thinking
                  : l.thinking,
              }));
              return;
            }
            case "assistant": {
              const blocks = readBlocks(payload);
              if (!blocks.length) return;
              // The complete message supersedes the deltas that built it.
              update((l) => ({
                blocks: [...l.blocks, ...blocks],
                text: "",
                thinking: "",
              }));
              return;
            }
            case "user": {
              const results = readToolResults(payload);
              if (!results.length) return;
              setTools((prev) => {
                const next = { ...prev };
                for (const r of results) {
                  next[r.toolUseId] = {
                    ...next[r.toolUseId],
                    result: r.content,
                    isError: r.isError,
                  };
                }
                return next;
              });
              return;
            }
            case "tool_progress": {
              const p = readToolProgress(payload);
              if (!p) return;
              const label =
                p.message ??
                (p.progress === undefined
                  ? undefined
                  : `${Math.round(p.progress)}%`);
              if (!label) return;
              setTools((prev) => ({
                ...prev,
                [p.toolUseId]: { ...prev[p.toolUseId], progress: label },
              }));
              return;
            }
            case "system": {
              const init = readSystemInit(payload);
              if (init) setSystem(init);
              return;
            }
            case "result": {
              const r = readResult(payload);
              resultRef.current = r;
              setResult(r);
              if (r.isError) {
                setError(r.text || "The agent reported an error.");
              }
              return;
            }
            case "user_input_required": {
              const questions = readQuestions(payload);
              setPrompt({ kind: "question", questions });
              return;
            }
            case "permission_request":
              setPrompt({ kind: "permission", request: readPermission(payload) });
              return;
            case "error":
              setError(readError(payload) ?? "The stream failed.");
              return;
            default:
              // New event types are added server-side; ignoring them is the
              // documented contract.
              return;
          }
        },
        onError: (err) => {
          setError(describeError(err));
          finish();
        },
        onDone: finish,
      });
    },
    [update]
  );

  const stop = useCallback(() => {
    const id = chatId;
    if (!id) return;
    if (stopping) {
      // Second press: the interrupt did not land, drop the connection.
      abortRef.current?.();
      return;
    }
    setStopping(true);
    api.post(`/chats/${id}/stop`).catch((err: unknown) => {
      setError(describeError(err));
      abortRef.current?.();
    });
  }, [chatId, stopping]);

  const answer = useCallback(
    async (text: string) => {
      if (!chatId) return;
      setPrompt(null);
      try {
        await api.post(`/chats/${chatId}/input`, { answer: text });
      } catch (err) {
        setError(describeError(err));
      }
    },
    [chatId]
  );

  const decide = useCallback(
    async (allow: boolean) => {
      if (!chatId) return;
      setPrompt(null);
      try {
        await api.post(`/chats/${chatId}/permission`, { allow });
      } catch (err) {
        setError(describeError(err));
      }
    },
    [chatId]
  );

  const reset = useCallback(() => {
    setTools({});
    setResult(null);
    setSystem(null);
    setError(undefined);
  }, []);

  return {
    chatId,
    live,
    prompt,
    tools,
    result,
    system,
    error,
    stopping,
    start,
    stop,
    answer,
    decide,
    reset,
    dismissError: useCallback(() => setError(undefined), []),
  };
}

function composeMessage(
  live: Live | null,
  result: TurnResult | null
): ChatMessage | null {
  const blocks: MessageBlock[] = live ? [...live.blocks] : [];
  if (live?.thinking) blocks.push({ type: "thinking", text: live.thinking });
  if (live?.text) blocks.push({ type: "text", text: live.text });

  const content =
    blocks
      .filter((b) => b.type === "text")
      .map((b) => b.text ?? "")
      .join("\n\n")
      .trim() || (result?.text ?? "");

  if (!blocks.length && !content) return null;
  return {
    role: "assistant",
    content,
    timestamp: new Date().toISOString(),
    blocks,
  };
}
