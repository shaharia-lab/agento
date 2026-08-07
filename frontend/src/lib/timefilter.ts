export type TimePreset = 'all' | '1h' | '24h' | '7d' | '30d' | 'custom'

export interface TimeRange {
  from: Date | null
  to: Date | null
}

const PRESET_DURATIONS_MS: Partial<Record<TimePreset, number>> = {
  '1h': 60 * 60 * 1000,
  '24h': 24 * 60 * 60 * 1000,
  '7d': 7 * 24 * 60 * 60 * 1000,
  '30d': 30 * 24 * 60 * 60 * 1000,
}

/**
 * Resolves a preset (plus optional custom bounds) into an absolute range.
 * Presets are relative to `now`; a null bound means open-ended.
 */
export function resolvePresetRange(
  preset: TimePreset,
  customFrom?: string,
  customTo?: string,
  now: Date = new Date(),
): TimeRange {
  if (preset === 'all') return { from: null, to: null }
  if (preset === 'custom') {
    return {
      from: customFrom ? new Date(customFrom) : null,
      to: customTo ? new Date(customTo) : null,
    }
  }
  return { from: new Date(now.getTime() - PRESET_DURATIONS_MS[preset]!), to: null }
}

/**
 * Returns true when the session activity window [startISO, lastActivityISO]
 * overlaps the range [from, to]. Null bounds are open-ended.
 */
export function overlapsRange(
  startISO: string,
  lastActivityISO: string,
  from: Date | null,
  to: Date | null,
): boolean {
  const start = new Date(startISO).getTime()
  const lastActivity = new Date(lastActivityISO).getTime()
  if (Number.isNaN(start) || Number.isNaN(lastActivity)) return false
  if (to !== null && start > to.getTime()) return false
  if (from !== null && lastActivity < from.getTime()) return false
  return true
}
