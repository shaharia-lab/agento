import { describe, it, expect } from 'vitest'
import { applyThinkingDelta, applyTextDelta, applyToolUseBlocks } from './streamingBlocks'
import type { MessageBlock, SDKContentBlock } from '@/types'

describe('applyThinkingDelta', () => {
  it('creates a new thinking block when blocks are empty', () => {
    const result = applyThinkingDelta([], 'hello')
    expect(result).toEqual([{ type: 'thinking', text: 'hello' }])
  })

  it('appends to the last thinking block', () => {
    const blocks: MessageBlock[] = [{ type: 'thinking', text: 'hello' }]
    const result = applyThinkingDelta(blocks, ' world')
    expect(result).toEqual([{ type: 'thinking', text: 'hello world' }])
  })

  it('creates a new thinking block when the last block is text', () => {
    const blocks: MessageBlock[] = [{ type: 'text', text: 'some text' }]
    const result = applyThinkingDelta(blocks, 'thinking')
    expect(result).toEqual([
      { type: 'text', text: 'some text' },
      { type: 'thinking', text: 'thinking' },
    ])
  })

  it('does not mutate the original array', () => {
    const blocks: MessageBlock[] = [{ type: 'thinking', text: 'a' }]
    const result = applyThinkingDelta(blocks, 'b')
    expect(result).not.toBe(blocks)
    expect(blocks[0]).toEqual({ type: 'thinking', text: 'a' })
  })
})

describe('applyTextDelta', () => {
  it('creates a new text block when blocks are empty', () => {
    const result = applyTextDelta([], 'hello')
    expect(result).toEqual([{ type: 'text', text: 'hello' }])
  })

  it('appends to the last text block', () => {
    const blocks: MessageBlock[] = [{ type: 'text', text: 'hello' }]
    const result = applyTextDelta(blocks, ' world')
    expect(result).toEqual([{ type: 'text', text: 'hello world' }])
  })

  it('creates a new text block when the last block is tool_use', () => {
    const blocks: MessageBlock[] = [
      { type: 'tool_use', name: 'Bash', id: '1', input: { command: 'ls' } },
    ]
    const result = applyTextDelta(blocks, 'output')
    expect(result).toEqual([
      { type: 'tool_use', name: 'Bash', id: '1', input: { command: 'ls' } },
      { type: 'text', text: 'output' },
    ])
  })

  it('does not mutate the original array', () => {
    const blocks: MessageBlock[] = [{ type: 'text', text: 'a' }]
    const result = applyTextDelta(blocks, 'b')
    expect(result).not.toBe(blocks)
    expect(blocks[0]).toEqual({ type: 'text', text: 'a' })
  })
})

describe('applyToolUseBlocks', () => {
  it('appends tool_use blocks from SDK content', () => {
    const sdkBlocks: SDKContentBlock[] = [
      { type: 'tool_use', id: 'tool-1', name: 'Read', input: { file_path: '/tmp/a' } },
    ]
    const result = applyToolUseBlocks([], sdkBlocks)
    expect(result).toEqual([
      { type: 'tool_use', id: 'tool-1', name: 'Read', input: { file_path: '/tmp/a' } },
    ])
  })

  it('filters out non-tool_use blocks', () => {
    const sdkBlocks: SDKContentBlock[] = [
      { type: 'text', text: 'hello' },
      { type: 'tool_use', id: 'tool-1', name: 'Bash', input: { command: 'ls' } },
      { type: 'thinking', thinking: 'hmm' },
    ]
    const result = applyToolUseBlocks([], sdkBlocks)
    expect(result).toHaveLength(1)
    expect(result[0]).toEqual({
      type: 'tool_use',
      id: 'tool-1',
      name: 'Bash',
      input: { command: 'ls' },
    })
  })

  it('filters out tool_use blocks without a name', () => {
    const sdkBlocks: SDKContentBlock[] = [
      { type: 'tool_use', id: 'tool-1' }, // no name
    ]
    const result = applyToolUseBlocks([], sdkBlocks)
    expect(result).toEqual([])
  })

  it('returns the original array reference when no tool_use blocks found', () => {
    const blocks: MessageBlock[] = [{ type: 'text', text: 'hello' }]
    const result = applyToolUseBlocks(blocks, [{ type: 'text', text: 'foo' }])
    expect(result).toBe(blocks)
  })

  it('appends after existing blocks preserving order', () => {
    const blocks: MessageBlock[] = [{ type: 'text', text: 'before' }]
    const sdkBlocks: SDKContentBlock[] = [
      { type: 'tool_use', id: 'tool-1', name: 'Bash', input: { command: 'ls' } },
    ]
    const result = applyToolUseBlocks(blocks, sdkBlocks)
    expect(result).toEqual([
      { type: 'text', text: 'before' },
      { type: 'tool_use', id: 'tool-1', name: 'Bash', input: { command: 'ls' } },
    ])
  })
})

describe('streaming block ordering (integration)', () => {
  it('preserves chronological order: text → tool → text', () => {
    let blocks: MessageBlock[] = []

    // First: some text arrives
    blocks = applyTextDelta(blocks, 'Let me read the file.')

    // Then: a tool call arrives
    blocks = applyToolUseBlocks(blocks, [
      { type: 'tool_use', id: 'tool-1', name: 'Read', input: { file_path: '/tmp/a.txt' } },
    ])

    // Then: more text arrives
    blocks = applyTextDelta(blocks, 'Here is the content.')

    expect(blocks).toEqual([
      { type: 'text', text: 'Let me read the file.' },
      { type: 'tool_use', id: 'tool-1', name: 'Read', input: { file_path: '/tmp/a.txt' } },
      { type: 'text', text: 'Here is the content.' },
    ])

    // Verify order: text, tool_use, text
    expect(blocks.map(b => b.type)).toEqual(['text', 'tool_use', 'text'])
  })

  it('preserves chronological order: thinking → text → tool → tool → text', () => {
    let blocks: MessageBlock[] = []

    // Thinking first
    blocks = applyThinkingDelta(blocks, 'Let me think...')
    blocks = applyThinkingDelta(blocks, ' about this.')

    // Then text
    blocks = applyTextDelta(blocks, 'I will check two files.')

    // Then two tool calls
    blocks = applyToolUseBlocks(blocks, [
      { type: 'tool_use', id: 'tool-1', name: 'Read', input: { file_path: '/a.txt' } },
    ])
    blocks = applyToolUseBlocks(blocks, [
      { type: 'tool_use', id: 'tool-2', name: 'Read', input: { file_path: '/b.txt' } },
    ])

    // Then more text
    blocks = applyTextDelta(blocks, 'Both files look good.')

    expect(blocks.map(b => b.type)).toEqual(['thinking', 'text', 'tool_use', 'tool_use', 'text'])

    // Verify thinking was accumulated correctly
    expect(blocks[0].type).toBe('thinking')
    if (blocks[0].type === 'thinking') {
      expect(blocks[0].text).toBe('Let me think... about this.')
    }
  })

  it('preserves order: tool → text → tool → text (no initial text)', () => {
    let blocks: MessageBlock[] = []

    // Tool call first (no preceding text)
    blocks = applyToolUseBlocks(blocks, [
      { type: 'tool_use', id: 'tool-1', name: 'Bash', input: { command: 'pwd' } },
    ])

    // Then text
    blocks = applyTextDelta(blocks, 'Current directory is /home.')

    // Then another tool
    blocks = applyToolUseBlocks(blocks, [
      { type: 'tool_use', id: 'tool-2', name: 'Bash', input: { command: 'ls' } },
    ])

    // Then more text
    blocks = applyTextDelta(blocks, 'Files listed above.')

    expect(blocks.map(b => b.type)).toEqual(['tool_use', 'text', 'tool_use', 'text'])
  })

  it('consecutive text deltas merge into a single text block', () => {
    let blocks: MessageBlock[] = []
    blocks = applyTextDelta(blocks, 'Hello')
    blocks = applyTextDelta(blocks, ' ')
    blocks = applyTextDelta(blocks, 'world')

    expect(blocks).toEqual([{ type: 'text', text: 'Hello world' }])
  })

  it('text deltas after a tool create a new separate text block', () => {
    let blocks: MessageBlock[] = []
    blocks = applyTextDelta(blocks, 'before')
    blocks = applyToolUseBlocks(blocks, [{ type: 'tool_use', id: 't1', name: 'Read', input: {} }])
    blocks = applyTextDelta(blocks, 'after')

    // Must be 3 separate blocks, not 2 (text must not merge across a tool_use)
    expect(blocks).toHaveLength(3)
    expect(blocks[0]).toEqual({ type: 'text', text: 'before' })
    expect(blocks[2]).toEqual({ type: 'text', text: 'after' })
  })
})
