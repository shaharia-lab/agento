import { useEffect, useRef } from "react";
import { Icon } from "../../lib/icons";
import { MOD } from "../../lib/tauri";

export function Composer({
  value,
  onChange,
  onSend,
  placeholder,
  busy,
  stopping,
  onStop,
  meta,
}: {
  value: string;
  onChange(v: string): void;
  onSend(): void;
  placeholder: string;
  busy: boolean;
  stopping: boolean;
  onStop(): void;
  meta?: React.ReactNode;
}) {
  const box = useRef<HTMLTextAreaElement>(null);

  // Grow with the draft rather than scrolling a two-line box.
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 200)}px`;
  }, [value]);

  return (
    <div className="composer">
      <div className="composer__inner">
        <textarea
          ref={box}
          className="composer__input"
          placeholder={placeholder}
          value={value}
          rows={1}
          disabled={busy}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
              e.preventDefault();
              if (!busy && value.trim()) onSend();
            }
          }}
        />
        <div className="composer__bar">
          {meta}
          {busy ? (
            <>
              <div className="composer__hint">
                {stopping ? "Stopping…" : "Agent is working"}
              </div>
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
              <div className="composer__hint">
                <span className="kbd">{MOD} ↵</span>
                to send
              </div>
              <button
                className="sendbtn"
                disabled={!value.trim()}
                onClick={onSend}
                title="Send"
              >
                <Icon name="send" size={14} />
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
