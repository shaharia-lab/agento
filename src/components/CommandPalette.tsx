import { useEffect, useMemo, useRef, useState } from "react";
import { Icon, type IconName } from "../lib/icons";

export interface Command {
  id: string;
  label: string;
  group: string;
  icon: IconName;
  shortcut?: string;
  run(): void;
}

/**
 * ⌘K palette. A desktop app is expected to be fully drivable from the keyboard;
 * this is the entry point for that.
 */
export function CommandPalette({
  commands,
  onClose,
}: {
  commands: Command[];
  onClose(): void;
}) {
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const results = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return commands;
    return commands.filter(
      (c) =>
        c.label.toLowerCase().includes(q) || c.group.toLowerCase().includes(q)
    );
  }, [query, commands]);

  useEffect(() => setIndex(0), [query]);

  // Keep the highlighted row inside the scroll viewport as the user arrows down.
  useEffect(() => {
    listRef.current
      ?.querySelector<HTMLElement>(".palette__item--active")
      ?.scrollIntoView({ block: "nearest" });
  }, [index]);

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setIndex((i) => (results.length ? (i + 1) % results.length : 0));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setIndex((i) =>
        results.length ? (i - 1 + results.length) % results.length : 0
      );
    } else if (e.key === "Enter") {
      e.preventDefault();
      const chosen = results[index];
      if (chosen) {
        chosen.run();
        onClose();
      }
    }
  };

  return (
    <div className="overlay" onMouseDown={onClose}>
      <div
        className="palette"
        onMouseDown={(e) => e.stopPropagation()}
        onKeyDown={onKeyDown}
      >
        <div className="palette__input">
          <Icon name="search" size={18} />
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search commands, chats and agents…"
            spellCheck={false}
          />
          <span className="kbd">esc</span>
        </div>

        <div className="palette__list scroll" ref={listRef}>
          {results.length === 0 ? (
            <div
              style={{
                padding: "var(--sp-8)",
                textAlign: "center",
                color: "var(--fg-tertiary)",
              }}
            >
              No matching commands
            </div>
          ) : (
            results.map((c, i) => (
              <button
                key={c.id}
                className={`palette__item ${
                  i === index ? "palette__item--active" : ""
                }`}
                onMouseEnter={() => setIndex(i)}
                onClick={() => {
                  c.run();
                  onClose();
                }}
              >
                <Icon name={c.icon} />
                <span className="truncate">{c.label}</span>
                <span className="palette__group">{c.group}</span>
                {c.shortcut && <span className="kbd">{c.shortcut}</span>}
              </button>
            ))
          )}
        </div>
      </div>
    </div>
  );
}
