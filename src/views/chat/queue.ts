/* ============================================================================
   The composer's message queue (#723).

   While a turn runs the composer does not lock: a send becomes a queued item,
   and when the turn ends the whole queue goes out as one user message. The
   queue is view state only — one per chat, in memory, gone on restart.
   ========================================================================== */

export interface QueueItem {
  id: string;
  text: string;
}

/** The queue key of a chat that has not been created yet. */
export const DRAFT_KEY = "__draft__";

let seq = 0;

export function queueItem(text: string): QueueItem {
  seq += 1;
  return { id: `q${seq}`, text };
}

/** One user message from many: queue order, blank line between items. */
export function mergeQueue(items: QueueItem[]): string {
  return items
    .map((i) => i.text.trim())
    .filter(Boolean)
    .join("\n\n");
}

/** Move one item by `delta` places, clamped to the list. */
export function moveItem(
  items: QueueItem[],
  id: string,
  delta: number
): QueueItem[] {
  const from = items.findIndex((i) => i.id === id);
  if (from < 0) return items;
  const to = Math.max(0, Math.min(items.length - 1, from + delta));
  if (to === from) return items;
  const next = [...items];
  const [item] = next.splice(from, 1);
  next.splice(to, 0, item);
  return next;
}
