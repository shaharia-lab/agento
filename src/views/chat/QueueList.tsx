import { useEffect, useRef, useState } from "react";
import { Icon } from "../../lib/icons";
import type { QueueItem } from "./queue";

/**
 * The queued messages above the composer's input (#723). Deleting a card is a
 * plain × with no confirmation: it is an unsent draft, not a stored record.
 */
export function QueueList({
  items,
  editing,
  onEditing,
  onEdit,
  onDelete,
  onMove,
  onSendNow,
}: {
  items: QueueItem[];
  /** The card open for editing, owned by the composer so ↑ can open one. */
  editing: string | null;
  onEditing(id: string | null): void;
  onEdit(id: string, text: string): void;
  onDelete(id: string): void;
  onMove(id: string, delta: number): void;
  onSendNow?(id: string): void;
}) {
  if (!items.length) return null;
  return (
    <div className="queue">
      <div className="queue__head">
        {items.length} queued
        <span className="queue__help">
          sent as one message when the agent finishes
        </span>
      </div>
      {items.map((item) =>
        item.id === editing ? (
          <QueueEditor
            key={item.id}
            text={item.text}
            onSave={(text) => {
              if (text.trim()) onEdit(item.id, text);
              else onDelete(item.id);
              onEditing(null);
            }}
            onCancel={() => onEditing(null)}
          />
        ) : (
          <div key={item.id} className="queue__card">
            <button
              className="queue__text truncate"
              title="Edit · ⌥↑ ⌥↓ to reorder"
              onClick={() => onEditing(item.id)}
              onKeyDown={(e) => {
                if (!e.altKey) return;
                if (e.key === "ArrowUp" || e.key === "ArrowDown") {
                  e.preventDefault();
                  onMove(item.id, e.key === "ArrowUp" ? -1 : 1);
                }
              }}
            >
              {item.text}
            </button>
            {onSendNow && (
              <button
                className="iconbtn queue__btn"
                title="Stop the agent and send this now"
                onClick={() => onSendNow(item.id)}
              >
                <Icon name="send" size={12} />
              </button>
            )}
            <button
              className="iconbtn queue__btn"
              title="Remove from queue"
              onClick={() => onDelete(item.id)}
            >
              <Icon name="close" size={12} />
            </button>
          </div>
        )
      )}
    </div>
  );
}

function QueueEditor({
  text,
  onSave,
  onCancel,
}: {
  text: string;
  onSave(text: string): void;
  onCancel(): void;
}) {
  const [value, setValue] = useState(text);
  const box = useRef<HTMLTextAreaElement>(null);
  // Enter or Esc unmounts the editor, and a focused element being removed may
  // still blur — without this the cancelled edit would be saved on the way out.
  const done = useRef(false);
  const finish = (save: boolean) => {
    if (done.current) return;
    done.current = true;
    if (save) onSave(value);
    else onCancel();
  };
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    el.focus();
    el.setSelectionRange(el.value.length, el.value.length);
  }, []);
  return (
    <textarea
      ref={box}
      className="queue__edit"
      value={value}
      rows={Math.min(6, value.split("\n").length)}
      onChange={(e) => setValue(e.target.value)}
      onBlur={() => finish(true)}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.preventDefault();
          e.stopPropagation();
          finish(false);
        } else if (e.key === "Enter" && !e.shiftKey) {
          e.preventDefault();
          finish(true);
        }
      }}
    />
  );
}
