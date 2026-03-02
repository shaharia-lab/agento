import type { MessageBlock, SDKContentBlock } from '@/types'

let _blockKeyCounter = 0
function nextBlockKey(): string {
  return `k${++_blockKeyCounter}`
}

/**
 * Applies a thinking delta to the block list.
 * If the last block is a thinking block, appends to it; otherwise creates a new one.
 * Returns a new array (safe for React state).
 */
export function applyThinkingDelta(blocks: MessageBlock[], thinking: string): MessageBlock[] {
  const result = blocks.slice()
  const last = result[result.length - 1]
  if (last?.type === 'thinking') {
    result[result.length - 1] = { ...last, text: last.text + thinking }
  } else {
    result.push({ type: 'thinking', text: thinking, _key: nextBlockKey() })
  }
  return result
}

/**
 * Applies a text delta to the block list.
 * If the last block is a text block, appends to it; otherwise creates a new one.
 * Returns a new array (safe for React state).
 */
export function applyTextDelta(blocks: MessageBlock[], text: string): MessageBlock[] {
  const result = blocks.slice()
  const last = result[result.length - 1]
  if (last?.type === 'text') {
    result[result.length - 1] = { ...last, text: last.text + text }
  } else {
    result.push({ type: 'text', text, _key: nextBlockKey() })
  }
  return result
}

/**
 * Appends completed tool_use blocks from an assistant event.
 * Returns a new array (safe for React state).
 */
export function applyToolUseBlocks(
  blocks: MessageBlock[],
  sdkBlocks: SDKContentBlock[],
): MessageBlock[] {
  const toolUseBlocks = sdkBlocks.filter(b => b.type === 'tool_use' && b.name)
  if (toolUseBlocks.length === 0) return blocks
  const newBlocks: MessageBlock[] = toolUseBlocks.map(b => ({
    type: 'tool_use' as const,
    id: b.id,
    name: b.name!,
    input: b.input,
  }))
  return [...blocks, ...newBlocks]
}
