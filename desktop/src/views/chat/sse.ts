/* ============================================================================
   SSE payload narrowing.

   The server forwards the agent CLI's own JSON lines verbatim, so nothing here
   may assume a field exists — every value is probed rather than cast.
   ========================================================================== */

import type { MessageBlock } from "../../lib/types";

export function parseData(data: string): unknown {
  try {
    return JSON.parse(data) as unknown;
  } catch {
    return undefined;
  }
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function pick(v: unknown, ...path: string[]): unknown {
  let cur = v;
  for (const key of path) {
    if (!isRecord(cur)) return undefined;
    cur = cur[key];
  }
  return cur;
}

function str(v: unknown): string | undefined {
  return typeof v === "string" ? v : undefined;
}

function num(v: unknown): number | undefined {
  return typeof v === "number" && isFinite(v) ? v : undefined;
}

function arr(v: unknown): unknown[] {
  return Array.isArray(v) ? v : [];
}

/* --- stream_event -------------------------------------------------------- */

export interface Delta {
  text?: string;
  thinking?: string;
}

export function readDelta(payload: unknown): Delta | undefined {
  const delta = pick(payload, "event", "delta");
  if (!isRecord(delta)) return undefined;
  const text = str(delta.text);
  const thinking = str(delta.thinking);
  if (text === undefined && thinking === undefined) return undefined;
  return { text, thinking };
}

/* --- assistant ----------------------------------------------------------- */

/**
 * Blocks arrive with the thinking text under `thinking`, but storage keeps it
 * under `text` — normalising here means one renderer serves both live and
 * replayed transcripts.
 */
export function readBlocks(payload: unknown): MessageBlock[] {
  const out: MessageBlock[] = [];
  for (const raw of arr(pick(payload, "message", "content"))) {
    if (!isRecord(raw)) continue;
    const type = str(raw.type);
    if (!type) continue;
    if (type === "thinking") {
      out.push({ type, text: str(raw.thinking) ?? str(raw.text) ?? "" });
    } else if (type === "text") {
      out.push({ type, text: str(raw.text) ?? "" });
    } else if (type === "tool_use") {
      out.push({
        type,
        id: str(raw.id),
        name: str(raw.name) ?? "tool",
        input: raw.input,
      });
    }
  }
  return out;
}

/* --- user (tool results) ------------------------------------------------- */

export interface ToolResult {
  toolUseId: string;
  content: string;
  isError: boolean;
}

export function readToolResults(payload: unknown): ToolResult[] {
  const out: ToolResult[] = [];
  for (const raw of arr(pick(payload, "message", "content"))) {
    if (!isRecord(raw) || raw.type !== "tool_result") continue;
    const id = str(raw.tool_use_id);
    if (!id) continue;
    out.push({
      toolUseId: id,
      content: flatten(raw.content),
      isError: raw.is_error === true,
    });
  }
  return out;
}

/** Tool result content is a string on some tools and a block array on others. */
function flatten(v: unknown): string {
  if (typeof v === "string") return v;
  if (Array.isArray(v)) {
    return v
      .map((item) => (isRecord(item) ? str(item.text) ?? "" : String(item)))
      .filter(Boolean)
      .join("\n");
  }
  if (v === undefined || v === null) return "";
  return JSON.stringify(v, null, 2);
}

/* --- tool_progress ------------------------------------------------------- */

export interface ToolProgress {
  toolUseId: string;
  message?: string;
  progress?: number;
}

export function readToolProgress(payload: unknown): ToolProgress | undefined {
  if (!isRecord(payload)) return undefined;
  const id = str(payload.tool_use_id);
  if (!id) return undefined;
  return {
    toolUseId: id,
    message: str(payload.message),
    progress: num(payload.progress),
  };
}

/* --- result -------------------------------------------------------------- */

export interface TurnResult {
  text: string;
  costUsd?: number;
  inputTokens?: number;
  outputTokens?: number;
  cacheReadTokens?: number;
  cacheCreationTokens?: number;
  isError: boolean;
  durationMs?: number;
  numTurns?: number;
  sessionId?: string;
}

export function readResult(payload: unknown): TurnResult {
  return {
    text: str(pick(payload, "result")) ?? "",
    costUsd: num(pick(payload, "total_cost_usd")),
    inputTokens: num(pick(payload, "usage", "input_tokens")),
    outputTokens: num(pick(payload, "usage", "output_tokens")),
    cacheReadTokens: num(pick(payload, "usage", "cache_read_input_tokens")),
    cacheCreationTokens: num(
      pick(payload, "usage", "cache_creation_input_tokens")
    ),
    isError: pick(payload, "is_error") === true,
    durationMs: num(pick(payload, "duration_ms")),
    numTurns: num(pick(payload, "num_turns")),
    sessionId: str(pick(payload, "session_id")),
  };
}

/* --- system -------------------------------------------------------------- */

export interface SystemInit {
  model?: string;
  cwd?: string;
  permissionMode?: string;
  tools: string[];
}

export function readSystemInit(payload: unknown): SystemInit | undefined {
  if (!isRecord(payload) || payload.subtype !== "init") return undefined;
  return {
    model: str(payload.model),
    cwd: str(payload.cwd),
    permissionMode: str(payload.permissionMode),
    tools: arr(payload.tools).filter((t): t is string => typeof t === "string"),
  };
}

/* --- user_input_required ------------------------------------------------- */

export interface QuestionOption {
  label: string;
  description?: string;
}

export interface QuestionItem {
  question: string;
  header?: string;
  multiSelect: boolean;
  options: QuestionOption[];
}

export function readQuestions(payload: unknown): QuestionItem[] {
  const out: QuestionItem[] = [];
  for (const raw of arr(pick(payload, "input", "questions"))) {
    if (!isRecord(raw)) continue;
    out.push({
      question: str(raw.question) ?? "",
      header: str(raw.header),
      multiSelect: raw.multiSelect === true,
      options: arr(raw.options)
        .map((o): QuestionOption | undefined => {
          if (typeof o === "string") return { label: o };
          if (!isRecord(o)) return undefined;
          const label = str(o.label);
          return label ? { label, description: str(o.description) } : undefined;
        })
        .filter((o): o is QuestionOption => o !== undefined),
    });
  }
  return out;
}

/* --- permission_request -------------------------------------------------- */

export interface PermissionRequest {
  toolName: string;
  input: unknown;
}

export function readPermission(payload: unknown): PermissionRequest {
  return {
    toolName: str(pick(payload, "tool_name")) ?? "tool",
    input: pick(payload, "input"),
  };
}

/* --- error --------------------------------------------------------------- */

export function readError(payload: unknown): string | undefined {
  if (typeof payload === "string") return payload;
  return str(pick(payload, "error"));
}

/* --- shared helpers ------------------------------------------------------ */

export function summarizeInput(input: unknown): string {
  if (input === undefined || input === null) return "";
  if (typeof input === "string") return input;
  if (!isRecord(input)) return String(input);

  // Tool inputs lead with the field that identifies the work: a command, a
  // path, a query. Showing that beats showing `{"a":1,…}`.
  for (const key of ["command", "file_path", "path", "pattern", "query", "url", "prompt", "description"]) {
    const v = str(input[key]);
    if (v) return v;
  }
  return JSON.stringify(input);
}

export function prettyJson(v: unknown): string {
  if (v === undefined) return "";
  if (typeof v === "string") return v;
  try {
    return JSON.stringify(v, null, 2);
  } catch {
    return String(v);
  }
}
