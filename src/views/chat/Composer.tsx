import { useEffect, useRef, useState } from "react";
import { Icon } from "../../lib/icons";
import { IS_MAC, MOD } from "../../lib/tauri";
import { QueueList } from "./QueueList";
import type { QueueItem } from "./queue";

const NO_QUEUE: QueueItem[] = [];

export function Composer({
  value,
  onChange,
  onSend,
  placeholder,
  busy,
  stopping,
  onStop,
  meta,
  focusNonce = 0,
  queue = NO_QUEUE,
  onQueueEdit,
  onQueueDelete,
  onQueueMove,
  onSendNow,
}: {
  value: string;
  onChange(v: string): void;
  /**
   * Send the draft — or, while busy, queue it (#723). The composer never
   * locks; which of the two happens is the caller's decision, keyed on `busy`.
   */
  onSend(): void;
  placeholder: string;
  busy: boolean;
  stopping: boolean;
  onStop(): void;
  meta?: React.ReactNode;
  /**
   * Bumped by a caller that wants the cursor here. A nonce rather than
   * `autoFocus`, which React only applies on mount — this textarea survives a
   * change of chat, so the attribute would never fire again.
   */
  focusNonce?: number;
  queue?: QueueItem[];
  onQueueEdit?(id: string, text: string): void;
  onQueueDelete?(id: string): void;
  onQueueMove?(id: string, delta: number): void;
  /**
   * Stop the running turn and send one item — a queued card by id, or the
   * draft when called without one — as soon as the stream closes.
   */
  onSendNow?(id?: string): void;
}) {
  const box = useRef<HTMLTextAreaElement>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const hasDraft = value.trim() !== "";
  const queued = queue.length;

  // Deliberately not on mount: focus is only ever taken when somebody asked
  // for it, so opening the app or clicking through the list does not steal the
  // caret from wherever the user put it.
  const seenFocus = useRef(0);
  useEffect(() => {
    if (focusNonce === seenFocus.current) return;
    seenFocus.current = focusNonce;
    box.current?.focus();
  }, [focusNonce]);

  // Grow with the draft rather than scrolling a two-line box.
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 200)}px`;
  }, [value]);

  // An edited card may vanish underneath the editor (a flush empties the
  // queue); the editor must not outlive it.
  useEffect(() => {
    if (editing && !queue.some((i) => i.id === editing)) setEditing(null);
  }, [editing, queue]);

  const openCard = (id: string | null) => {
    setEditing(id);
    if (id === null) box.current?.focus();
  };

  return (
    <div className="composer">
      <div className="composer__inner">
        {onQueueEdit && onQueueDelete && onQueueMove && (
          <QueueList
            items={queue}
            editing={editing}
            onEditing={openCard}
            onEdit={onQueueEdit}
            onDelete={onQueueDelete}
            onMove={onQueueMove}
            onSendNow={busy ? onSendNow : undefined}
          />
        )}
        <textarea
          ref={box}
          className="composer__input"
          placeholder={
            busy
              ? `Queue a message — ${MOD} ↵, sent when the agent finishes`
              : placeholder
          }
          value={value}
          rows={1}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (IS_MAC ? e.metaKey : e.ctrlKey)) {
              e.preventDefault();
              if (busy && e.shiftKey && onSendNow) {
                if (hasDraft) onSendNow();
              } else if (hasDraft || (!busy && queued)) {
                onSend();
              }
              return;
            }
            // ↑ in an empty input opens the last queued card, the way a shell
            // recalls the last command.
            if (e.key === "ArrowUp" && !value && queued && !e.altKey) {
              e.preventDefault();
              setEditing(queue[queued - 1].id);
            }
          }}
        />
        <div className="composer__bar">
          {meta}
          {busy ? (
            <>
              <div className="composer__hint">
                {stopping ? "Stopping…" : ""}
              </div>
              <button
                className="btn composer__queue"
                disabled={!hasDraft}
                onClick={onSend}
                title={`Send when the agent finishes (${MOD} ↵)`}
              >
                Queue
              </button>
              <button
                className="btn btn--danger composer__stop"
                onClick={onStop}
                title={stopping ? "Force close the stream" : "Interrupt the agent"}
              >
                <Icon name="stop" size={12} />
                Stop
              </button>
            </>
          ) : (
            <>
              {/* "Send N queued" says it already, and the bar has no room for both. */}
              <div className="composer__hint">
                {!queued && (
                  <>
                    <span className="kbd">{MOD} ↵</span>
                    to send
                  </>
                )}
              </div>
              {queued ? (
                <button
                  className="btn btn--primary composer__queue"
                  onClick={onSend}
                  title={hasDraft ? "Send the queue and this draft" : "Send the queue"}
                >
                  Send {queued} queued
                </button>
              ) : (
                <button
                  className="sendbtn"
                  disabled={!hasDraft}
                  onClick={onSend}
                  title="Send"
                >
                  <Icon name="send" size={14} />
                </button>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
