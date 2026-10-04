import { tildePath } from "./format";
import type { ClaudeRetentionDir } from "./types";

/** `<dir>/settings.json`, the one file the retention read looks at. */
export function retentionFile(d: ClaudeRetentionDir): string {
  return `${tildePath(d.config_dir)}/settings.json`;
}

export function daysText(days: number): string {
  return days === 1 ? "1 day" : `${days} days`;
}

export interface RetentionPreset {
  days: number;
  label: string;
}

/**
 * Every value the retention prompt can write (#720). A fixed list on purpose:
 * it cannot be mistyped into a shortening, and there is no free number input.
 */
export const RETENTION_PRESETS: RetentionPreset[] = [
  { days: 90, label: "90 days" },
  { days: 365, label: "1 year" },
  { days: 1825, label: "5 years" },
];

/**
 * The presets a config dir may be extended to: those strictly above its
 * current value, so no offered choice can lower it. None for a dir that is
 * `unknown`, or at `0` — Claude Code rejects `0` and skips cleanup, so writing
 * any number there would start deletion. The route refuses both as well.
 */
export function extendOptions(d: ClaudeRetentionDir): RetentionPreset[] {
  const current = d.cleanup_period_days;
  if (d.source === "unknown" || current === undefined || current <= 0) return [];
  return RETENTION_PRESETS.filter((p) => p.days > current);
}
