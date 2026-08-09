import type { ClaudeMessage, ClaudeNormalizedBlock } from '../types'

/** Tools whose input names a file the session read or changed. */
const FILE_TOOLS = new Set(['Read', 'Write', 'Edit', 'MultiEdit', 'NotebookEdit'])

const str = (v: unknown): string => (typeof v === 'string' ? v : '')

/**
 * One-line summary of a tool call, shown beside the tool name on the collapsed
 * header. Falls back to the first string argument so an unrecognised tool — an
 * MCP tool, a new built-in — still says something rather than nothing.
 */
export function toolSummary(block: ClaudeNormalizedBlock): string {
  const input = block.input
  if (!input) return ''
  const name = block.name ?? ''

  if (FILE_TOOLS.has(name)) return str(input.file_path ?? input.filePath ?? input.notebook_path)
  if (name === 'Bash') return str(input.command)
  if (name === 'Glob' || name === 'Grep') return str(input.pattern ?? input.query)
  if (name === 'WebFetch' || name === 'WebSearch') return str(input.url ?? input.query)
  if (name === 'Task') return str(input.description)
  if (name === 'TodoWrite') return ''

  const firstString = Object.values(input).find(v => typeof v === 'string' && v.length > 0)
  return str(firstString)
}

/** Everything in a message that a transcript search should look at. */
export function messageText(msg: ClaudeMessage): string {
  const blocks = msg.blocks ?? []
  const fromBlocks = blocks
    .map(b => `${b.name ?? ''} ${b.text ?? ''} ${b.input ? JSON.stringify(b.input) : ''}`)
    .join(' ')
  return `${msg.content ?? ''} ${fromBlocks}`.toLowerCase()
}

/** Whether a message carries any renderable prose (as opposed to only tool calls). */
export function hasProse(msg: ClaudeMessage): boolean {
  if ((msg.content ?? '').trim()) return true
  return (msg.blocks ?? []).some(b => b.type === 'text' && (b.text ?? '').trim())
}

/** Whether a message issued at least one tool call. */
export function hasToolUse(msg: ClaudeMessage): boolean {
  return (msg.blocks ?? []).some(b => b.type === 'tool_use')
}

export interface ToolUsage {
  name: string
  count: number
}

/**
 * Tool call counts across the transcript, busiest first. Counted per `tool_use`
 * block rather than per message, because one assistant message can issue
 * several calls.
 */
export function toolUsage(messages: readonly ClaudeMessage[]): ToolUsage[] {
  const counts = new Map<string, number>()
  for (const msg of messages) {
    for (const b of msg.blocks ?? []) {
      if (b.type !== 'tool_use') continue
      const name = b.name ?? 'unknown'
      counts.set(name, (counts.get(name) ?? 0) + 1)
    }
  }
  return [...counts.entries()]
    .map(([name, count]) => ({ name, count }))
    .sort((a, b) => b.count - a.count || a.name.localeCompare(b.name))
}

export interface FileTouch {
  path: string
  count: number
}

/**
 * Files the session read or wrote, most-touched first.
 *
 * The transcript records no diff stat, so this reports how many times each path
 * was touched — a real figure — rather than an invented line count.
 */
export function filesTouched(messages: readonly ClaudeMessage[]): FileTouch[] {
  const counts = new Map<string, number>()
  for (const msg of messages) {
    for (const b of msg.blocks ?? []) {
      if (b.type !== 'tool_use' || !FILE_TOOLS.has(b.name ?? '')) continue
      const path = str(b.input?.file_path ?? b.input?.filePath ?? b.input?.notebook_path)
      if (!path) continue
      counts.set(path, (counts.get(path) ?? 0) + 1)
    }
  }
  return [...counts.entries()]
    .map(([path, count]) => ({ path, count }))
    .sort((a, b) => b.count - a.count || a.path.localeCompare(b.path))
}

/**
 * The last `segments` path components — "pages/ClaudeSessionsPage.tsx".
 *
 * A sidebar 300px wide cannot show a full repo path, and plain truncation cuts
 * off the filename, which is the only part that identifies the file.
 */
export function tailPath(path: string, segments = 2): string {
  const parts = path.split(/[/\\]/).filter(Boolean)
  if (parts.length <= segments) return path
  return parts.slice(-segments).join('/')
}

export interface OutlineEntry {
  uuid: string
  timestamp: string
  label: string
}

/**
 * The session's turning points: what the user asked, in order.
 *
 * Claude Code injects synthetic user events (command caveats, hook output,
 * tool results). Those are not things the user said, so listing them as
 * milestones would bury the four or five real instructions among dozens.
 */
export function outline(messages: readonly ClaudeMessage[]): OutlineEntry[] {
  const entries: OutlineEntry[] = []
  for (const msg of messages) {
    if ((msg.role ?? msg.type) !== 'user') continue
    const text = (msg.content ?? '').trim()
    if (!text || text.startsWith('<')) continue
    const firstLine = text.split('\n').find(l => l.trim()) ?? text
    entries.push({
      uuid: msg.uuid,
      timestamp: msg.timestamp,
      label: firstLine.trim().slice(0, 90),
    })
  }
  return entries
}
