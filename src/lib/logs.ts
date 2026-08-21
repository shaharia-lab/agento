/* ============================================================================
   The app log, read through the shell rather than through /api.

   `src-tauri/src/logs.rs` owns the files; its header says why they are Tauri
   commands and not API routes. The one consequence here is that everything
   below has no answer in a plain browser tab (`npm run dev`) — `available()`
   is what the pane checks before it renders anything, rather than each call
   inventing an empty result that would read as "nothing has been logged".
   ========================================================================== */

import { IS_TAURI } from "./tauri";

export interface LogFile {
  name: string;
  bytes: number;
  modified_ms: number | null;
  live: boolean;
}

export interface LogIndex {
  dir: string;
  /** Live file first, then archives newest to oldest. */
  files: LogFile[];
}

export interface LogChunk {
  text: string;
  start: number;
  /** Pass back as `from` to follow the file; always a line boundary. */
  next: number;
  size: number;
  /** Older lines exist above what came back. */
  truncated: boolean;
  /** Replaces the caller's buffer rather than extending it. */
  reset: boolean;
}

/** Whether this build can read the log at all. False in a browser tab. */
export const LOGS_AVAILABLE = IS_TAURI;

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export function logFiles(): Promise<LogIndex> {
  return invoke<LogIndex>("log_files");
}

/**
 * Read one file. With no `from` this is the tail; with one it is whatever has
 * been appended since — which is how following works, and why the caller keeps
 * `next` rather than re-reading the whole file every two seconds.
 */
export function readLog(opts: {
  name?: string;
  maxBytes?: number;
  from?: number;
}): Promise<LogChunk> {
  return invoke<LogChunk>("read_log", {
    name: opts.name ?? null,
    maxBytes: opts.maxBytes ?? null,
    from: opts.from ?? null,
  });
}

/** Write every log file into one at `dest`, oldest first. Returns bytes written. */
export function exportLogs(dest: string): Promise<number> {
  return invoke<number>("export_logs", { dest });
}

/**
 * Native save dialog. Returns the chosen path, or null when cancelled or when
 * running outside Tauri. `dialog:default` already covers `save`, so this needs
 * no capability change — unlike anything reached through the fs plugin, which
 * is not installed.
 */
export async function pickSavePath(defaultName: string): Promise<string | null> {
  if (!IS_TAURI) return null;
  const { save } = await import("@tauri-apps/plugin-dialog");
  const picked = await save({
    title: "Save a copy of the logs",
    defaultPath: defaultName,
    filters: [{ name: "Log", extensions: ["log"] }],
  });
  return typeof picked === "string" ? picked : null;
}

/* ============================================================================
   Parsing

   `tauri_plugin_log`'s default file format is what `lib.rs` writes:

       [2026-08-22][14:03:11][agento_lib::proxy][INFO] GET /api/agents 200 4ms

   **Target before level, and that is worth knowing**: the plugin has a second
   format with the two the other way round, installed by `.timezone_strategy()`
   — which `lib.rs` does not call, and which would silently swap the columns
   here if it ever did. So the two bracketed fields are told apart by whether
   one of them *is* a level name rather than by position, and neither ordering
   can break this. Measured against a running dev build, not read off the
   source.

   Anything that does not match at all is a continuation of the line before it
   — a panic backtrace, a multi-line error — and is kept with its parent rather
   than dropped, because the continuation is usually the interesting half.
   ========================================================================== */

export type LogLevel = "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE";

export const LEVELS: LogLevel[] = ["ERROR", "WARN", "INFO", "DEBUG", "TRACE"];

export interface LogEntry {
  /** Index in the parsed list; stable enough to key rows off. */
  id: number;
  level: LogLevel;
  /** `YYYY-MM-DD HH:MM:SS`, or "" for a line that carried no timestamp. */
  time: string;
  target: string;
  text: string;
}

const LINE =
  /^\[(\d{4}-\d{2}-\d{2})\]\[(\d{2}:\d{2}:\d{2})\]\[([^\]]*)\]\[([^\]]*)\]\s?([\s\S]*)$/;

function isLevel(raw: string): raw is LogLevel {
  return (LEVELS as string[]).includes(raw);
}

/**
 * Split raw log text into entries.
 *
 * `into` lets a follow append onto the list it already has without re-parsing
 * megabytes every two seconds; a continuation line arriving in a later chunk
 * then still lands on the entry it belongs to.
 */
export function parseLog(text: string, into: LogEntry[] = []): LogEntry[] {
  const out = into;
  let next = out.length ? out[out.length - 1].id + 1 : 0;

  for (const line of text.split("\n")) {
    if (line === "") continue;
    const m = LINE.exec(line);
    if (m) {
      // Whichever of the two fields names a level is the level; the other is
      // the target. A line carrying neither is treated as INFO from the field
      // that is not a level, which is what an unknown future format degrades
      // to rather than losing the message.
      const [a, b] = [m[3], m[4]];
      out.push({
        id: next++,
        time: `${m[1]} ${m[2]}`,
        level: isLevel(b) ? b : isLevel(a) ? a : "INFO",
        target: isLevel(b) ? a : b,
        text: m[5],
      });
    } else if (out.length) {
      // A continuation belongs to the entry above it, at that entry's level:
      // the second line of a panic is not an INFO line.
      const prev = out[out.length - 1];
      prev.text = `${prev.text}\n${line}`;
    } else {
      // The first line of a tail is routinely a fragment of one the read cut,
      // and there is nothing above it to attach to.
      out.push({ id: next++, time: "", level: "INFO", target: "", text: line });
    }
  }
  return out;
}

/** Human-readable byte count, matching the app's compact number style. */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
