/* ============================================================================
   Session export (#591), through the shell rather than through /api.

   `src-tauri/src/native/sessions/export.rs` owns the formats; its header says
   why this is a Tauri command. The shape here is `lib/logs.ts`'s: a native
   Save-As picker, then one `invoke` that writes to the picked path. Neither
   has an answer in a plain browser tab (`npm run dev`), so the panel checks
   `EXPORT_AVAILABLE` rather than failing on click.
   ========================================================================== */

import { IS_TAURI } from "./tauri";

export type ExportFormat = "markdown" | "jsonl" | "text";

/** The command's `options`; snake_case because serde reads it as-is. */
export interface ExportOptions {
  format: ExportFormat;
  include_reasoning: boolean;
  include_tool_calls: boolean;
  include_tool_results: boolean;
  include_system: boolean;
  include_metadata: boolean;
}

export interface ExportResult {
  path: string;
  bytes: number;
  attachments: number;
  attachments_dir?: string;
}

export const EXPORT_AVAILABLE = IS_TAURI;

export const FORMAT_EXT: Record<ExportFormat, string> = {
  markdown: "md",
  jsonl: "jsonl",
  text: "txt",
};

const FORMAT_FILTER: Record<ExportFormat, string> = {
  markdown: "Markdown",
  jsonl: "JSON Lines",
  text: "Text",
};

/** Every scope toggle off, which is what the panel opens with. */
export const DEFAULT_EXPORT_OPTIONS: ExportOptions = {
  format: "markdown",
  include_reasoning: false,
  include_tool_calls: false,
  include_tool_results: false,
  include_system: false,
  include_metadata: false,
};

/** `{session-title-slug}-{date}.{ext}`, the id standing in for no title. */
export function exportFileName(
  title: string | undefined,
  sessionId: string,
  format: ExportFormat,
  now = new Date()
): string {
  const slug =
    (title ?? "")
      .toLowerCase()
      .normalize("NFKD")
      .replace(/[̀-ͯ]/g, "")
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 60)
      .replace(/-+$/, "") || sessionId;
  const pad = (n: number) => String(n).padStart(2, "0");
  const date = `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(
    now.getDate()
  )}`;
  return `${slug}-${date}.${FORMAT_EXT[format]}`;
}

/**
 * Native save dialog. Returns the chosen path, or null when cancelled or when
 * running outside Tauri. `dialog:default` already covers `save`.
 */
export async function pickExportPath(
  defaultName: string,
  format: ExportFormat
): Promise<string | null> {
  if (!IS_TAURI) return null;
  const { save } = await import("@tauri-apps/plugin-dialog");
  const picked = await save({
    title: "Export session",
    defaultPath: defaultName,
    filters: [{ name: FORMAT_FILTER[format], extensions: [FORMAT_EXT[format]] }],
  });
  return typeof picked === "string" ? picked : null;
}

export async function exportSession(
  sessionId: string,
  destPath: string,
  options: ExportOptions
): Promise<ExportResult> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<ExportResult>("export_session", {
    sessionId,
    destPath,
    options,
  });
}
