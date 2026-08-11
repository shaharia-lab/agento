import { describe, it, expect } from 'vitest'
import type { ClaudeMessage, ClaudeNormalizedBlock } from '../types'
import {
  filesTouched,
  hasProse,
  hasToolUse,
  messageText,
  searchIndex,
  outline,
  tailPath,
  toolSummary,
  toolUsage,
} from './transcript'

const tool = (name: string, input: Record<string, unknown>): ClaudeNormalizedBlock => ({
  type: 'tool_use',
  name,
  input,
})

const msg = (over: Partial<ClaudeMessage>): ClaudeMessage =>
  ({
    uuid: 'u1',
    type: 'assistant',
    role: 'assistant',
    timestamp: '2026-08-09T10:00:00Z',
    ...over,
  }) as ClaudeMessage

describe('toolSummary', () => {
  it('names the file for file tools', () => {
    expect(toolSummary(tool('Edit', { file_path: '/a/b.ts' }))).toBe('/a/b.ts')
    expect(toolSummary(tool('NotebookEdit', { notebook_path: '/a/n.ipynb' }))).toBe('/a/n.ipynb')
  })

  it('uses the command, pattern, url or description per tool', () => {
    expect(toolSummary(tool('Bash', { command: 'ls -la' }))).toBe('ls -la')
    expect(toolSummary(tool('Grep', { pattern: 'foo' }))).toBe('foo')
    expect(toolSummary(tool('WebFetch', { url: 'https://x' }))).toBe('https://x')
    expect(toolSummary(tool('Task', { description: 'do it' }))).toBe('do it')
  })

  it('falls back to the first string argument for unknown tools', () => {
    expect(toolSummary(tool('mcp__x__y', { count: 3, target: 'thing' }))).toBe('thing')
  })

  it('is empty when there is nothing worth showing', () => {
    expect(toolSummary({ type: 'tool_use', name: 'Bash' })).toBe('')
    expect(toolSummary(tool('TodoWrite', { todos: [] }))).toBe('')
    expect(toolSummary(tool('Unknown', { n: 1 }))).toBe('')
  })
})

describe('hasProse / hasToolUse', () => {
  it('separates prose from tool-only turns', () => {
    const proseOnly = msg({ blocks: [{ type: 'text', text: 'hi' }] })
    const toolOnly = msg({ blocks: [tool('Bash', { command: 'ls' })] })
    expect(hasProse(proseOnly)).toBe(true)
    expect(hasToolUse(proseOnly)).toBe(false)
    expect(hasProse(toolOnly)).toBe(false)
    expect(hasToolUse(toolOnly)).toBe(true)
  })

  it('treats whitespace-only text as no prose', () => {
    expect(hasProse(msg({ blocks: [{ type: 'text', text: '  \n ' }] }))).toBe(false)
  })

  it('counts a user message body as prose', () => {
    expect(hasProse(msg({ role: 'user', content: 'do the thing' }))).toBe(true)
  })
})

describe('messageText', () => {
  it('searches content, block text and tool input alike, case-insensitively', () => {
    const m = msg({
      content: 'Outer',
      blocks: [{ type: 'text', text: 'Inner' }, tool('Bash', { command: 'RUN-ME' })],
    })
    const text = messageText(m)
    expect(text).toContain('outer')
    expect(text).toContain('inner')
    expect(text).toContain('run-me')
    expect(text).toContain('bash')
  })
})

describe('toolUsage', () => {
  it('counts per tool_use block, not per message, busiest first', () => {
    const messages = [
      msg({ blocks: [tool('Bash', {}), tool('Bash', {}), tool('Read', {})] }),
      msg({ blocks: [tool('Bash', {})] }),
    ]
    expect(toolUsage(messages)).toEqual([
      { name: 'Bash', count: 3 },
      { name: 'Read', count: 1 },
    ])
  })

  it('is empty for a transcript with no tools', () => {
    expect(toolUsage([msg({ blocks: [{ type: 'text', text: 'hi' }] })])).toEqual([])
  })
})

describe('filesTouched', () => {
  it('counts file-tool paths and ignores other tools', () => {
    const messages = [
      msg({
        blocks: [
          tool('Read', { file_path: '/a/x.ts' }),
          tool('Edit', { file_path: '/a/x.ts' }),
          tool('Bash', { command: 'rm /a/y.ts' }),
        ],
      }),
    ]
    expect(filesTouched(messages)).toEqual([{ path: '/a/x.ts', count: 2 }])
  })

  it('skips a file tool with no path rather than recording an empty one', () => {
    expect(filesTouched([msg({ blocks: [tool('Read', { offset: 1 })] })])).toEqual([])
  })
})

describe('outline', () => {
  it('lists only genuine user turns, first line, in order', () => {
    const messages = [
      msg({ uuid: 'a', role: 'user', content: 'First ask\nmore detail' }),
      msg({ uuid: 'b', role: 'assistant', blocks: [{ type: 'text', text: 'ok' }] }),
      msg({ uuid: 'c', role: 'user', content: '<local-command-caveat>noise' }),
      msg({ uuid: 'd', role: 'user', content: '   ' }),
      msg({ uuid: 'e', role: 'user', content: 'Second ask' }),
    ]
    expect(outline(messages).map(o => [o.uuid, o.label])).toEqual([
      ['a', 'First ask'],
      ['e', 'Second ask'],
    ])
  })

  it('truncates a long instruction', () => {
    const long = 'x'.repeat(200)
    expect(outline([msg({ role: 'user', content: long })])[0].label).toHaveLength(90)
  })
})

describe('tailPath', () => {
  it('keeps the last two segments', () => {
    expect(tailPath('/home/u/proj/src/pages/Page.tsx')).toBe('pages/Page.tsx')
  })

  it('returns short paths unchanged', () => {
    expect(tailPath('Page.tsx')).toBe('Page.tsx')
    expect(tailPath('src/Page.tsx')).toBe('src/Page.tsx')
  })

  it('handles Windows separators', () => {
    expect(tailPath('C:\\Users\\u\\src\\pages\\Page.tsx')).toBe('pages/Page.tsx')
  })
})

describe('searchIndex', () => {
  it('is aligned by index with the messages it was built from', () => {
    const messages = [
      msg({ content: 'first message' }),
      msg({ blocks: [tool('Read', { file_path: '/a/b.go' })] }),
      msg({ content: 'third' }),
    ]
    const index = searchIndex(messages)
    expect(index).toHaveLength(3)
    // The search box matches against this by position, so a shorter or
    // reordered array would silently search the wrong message.
    expect(index[0]).toContain('first message')
    expect(index[1]).toContain('/a/b.go')
    expect(index[2]).toContain('third')
  })

  it('produces exactly what messageText produces', () => {
    // Built once per transcript instead of once per keystroke per message, so
    // it has to be the same text — a divergence would make the search find
    // different results depending on how it was computed.
    const messages = [
      msg({ content: 'Hello' }),
      msg({ blocks: [tool('Bash', { command: 'ls -la' })] }),
    ]
    expect(searchIndex(messages)).toEqual(messages.map(messageText))
  })

  it('lowercases, so the search never has to', () => {
    expect(searchIndex([msg({ content: 'SHOUTING' })])[0]).toContain('shouting')
  })

  it('is empty for an empty transcript rather than throwing', () => {
    expect(searchIndex([])).toEqual([])
  })
})
