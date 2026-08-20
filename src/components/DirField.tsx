import { useCallback, useEffect, useState } from "react";
import { api, qs } from "../lib/api";
import { describeError, useResource } from "../lib/hooks";
import { tildePath } from "../lib/format";
import { Icon } from "../lib/icons";
import { IS_TAURI, pickDirectory } from "../lib/tauri";
import type { FSEntry } from "../lib/types";

/**
 * Directory picking, in the two forms every path field in the app needs.
 *
 * There are two pickers because neither covers the whole product on its own.
 * `pickDirectory` is the OS's own dialog and is what a desktop app should
 * offer — it browses the real filesystem, honours symlinks and bookmarks, and
 * costs no round trip. `DirBrowser` is an in-app listing over `GET /api/fs`,
 * which is Unix-only and cannot see outside what the backend will list.
 *
 * The native one is tried first and the in-app one is the fallback, and the
 * fallback is not only for `npm run dev` in a browser tab: the dialog plugin is
 * an IPC command, so it fails whenever the ACL does not cover the window's
 * origin — which is exactly the bug that made "Browse…" a no-op in every
 * release build before the `remote` block landed in
 * `src-tauri/capabilities/default.json`. A rejected `invoke` is an ordinary
 * promise rejection with no visible effect, so a picker that only tries the
 * native path fails **silently**, leaving a required field that can only be
 * typed. Falling back turns the worst case into a working, if plainer, picker.
 */
export function useDirPicker(): {
  /** Renders the modal when the native dialog was unavailable. */
  browser: React.ReactNode;
  /** Opens whichever picker works, then calls `onPick` with the chosen path. */
  browse(title: string, current: string, onPick: (path: string) => void): void;
} {
  const [open, setOpen] = useState<{
    start: string;
    onPick(path: string): void;
  } | null>(null);

  const browse = useCallback(
    (title: string, current: string, onPick: (path: string) => void) => {
      if (!IS_TAURI) {
        setOpen({ start: current, onPick });
        return;
      }
      pickDirectory(title, current).then(
        (picked) => {
          // `null` is a cancel, which must not open a second picker on top of
          // the one the user just dismissed.
          if (picked) onPick(picked);
        },
        (err) => {
          console.warn("native folder picker unavailable", err);
          setOpen({ start: current, onPick });
        }
      );
    },
    []
  );

  return {
    browse,
    browser: open ? (
      <DirBrowser
        start={open.start}
        onPick={(path) => {
          open.onPick(path);
          setOpen(null);
        }}
        onClose={() => setOpen(null)}
      />
    ) : null,
  };
}

/** A path input with a Browse button, which is every directory field here. */
export function DirField({
  value,
  onChange,
  title,
  placeholder,
  disabled,
  browse,
  compact,
}: {
  value: string;
  onChange(path: string): void;
  /** The native dialog's window title. */
  title: string;
  placeholder?: string;
  disabled?: boolean;
  browse: ReturnType<typeof useDirPicker>["browse"];
  /** Field-height variant for the New Chat strip. */
  compact?: boolean;
}) {
  return (
    <div className="row" style={{ gap: "var(--sp-3)", flex: 1, minWidth: 0 }}>
      <label
        className={`field ${compact ? "field--sm" : ""} ${disabled ? "field--locked" : ""}`}
        style={{ flex: 1, minWidth: 0 }}
      >
        <span className="field__icon">
          <Icon name="folder" size={compact ? 12 : 14} />
        </span>
        <input
          className="mono"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={placeholder}
          disabled={disabled}
          spellCheck={false}
        />
      </label>
      <button
        className={compact ? "btn" : "btn btn--lg"}
        disabled={disabled}
        title="Browse for a folder"
        onClick={() => browse(title, value, onChange)}
      >
        {!compact && <Icon name="folder" size={14} />}
        Browse…
      </button>
    </div>
  );
}

/* --- Directory browser ---------------------------------------------------- */

interface FSListing {
  path: string;
  parent: string;
  entries: FSEntry[] | null;
}

export function DirBrowser({
  start,
  onPick,
  onClose,
}: {
  start: string;
  onPick(path: string): void;
  onClose(): void;
}) {
  const [path, setPath] = useState(start || "~");
  const [creating, setCreating] = useState(false);
  const [newName, setNewName] = useState("");
  const [error, setError] = useState<string>();

  const listing = useResource(
    (signal) => api.get<FSListing>(`/fs${qs({ path })}`, signal),
    [path]
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const here = listing.data?.path ?? path;
  const entries = listing.data?.entries ?? [];

  async function mkdir() {
    const name = newName.trim();
    if (!name) return;
    setError(undefined);
    try {
      await api.post("/fs/mkdir", { path: `${here}/${name}` });
      setNewName("");
      setCreating(false);
      listing.reload();
    } catch (err) {
      setError(describeError(err));
    }
  }

  return (
    <div className="overlay" onMouseDown={onClose}>
      <div className="browser" onMouseDown={(e) => e.stopPropagation()}>
        <div className="browser__head">
          <button
            className="iconbtn"
            onClick={() => listing.data && setPath(listing.data.parent)}
            disabled={!listing.data || listing.data.parent === here}
            title="Parent directory"
          >
            <Icon name="arrowUp" size={14} />
          </button>
          <div className="browser__path" title={here}>
            {tildePath(here)}
          </div>
          <button className="iconbtn" onClick={listing.reload} title="Refresh">
            <Icon name="refresh" size={13} />
          </button>
        </div>

        <div className="browser__list scroll">
          {listing.error ? (
            <div className="msgline msgline--error" style={{ margin: "var(--sp-4)" }}>
              {listing.error}
            </div>
          ) : entries.length === 0 ? (
            <div
              style={{
                padding: "var(--sp-7)",
                textAlign: "center",
                fontSize: "var(--text-sm)",
                color: "var(--fg-tertiary)",
              }}
            >
              {listing.loading ? "Loading…" : "No sub-directories."}
            </div>
          ) : (
            entries.map((e) => (
              <button
                key={e.path}
                className="browser__row"
                onDoubleClick={() => setPath(e.path)}
                onClick={() => setPath(e.path)}
              >
                <Icon name="folder" size={14} />
                <span>{e.name}</span>
              </button>
            ))
          )}
        </div>

        {error && (
          <div className="msgline msgline--error" style={{ margin: "var(--sp-4)" }}>
            {error}
          </div>
        )}

        <div className="browser__foot">
          {creating ? (
            <>
              <label className="field field--sm" style={{ flex: 1 }}>
                <input
                  autoFocus
                  value={newName}
                  onChange={(ev) => setNewName(ev.target.value)}
                  onKeyDown={(ev) => ev.key === "Enter" && mkdir()}
                  placeholder="Folder name"
                  spellCheck={false}
                />
              </label>
              <button className="btn" onClick={mkdir}>
                Create
              </button>
              <button className="btn btn--ghost" onClick={() => setCreating(false)}>
                Cancel
              </button>
            </>
          ) : (
            <>
              <button className="btn" onClick={() => setCreating(true)}>
                <Icon name="plus" size={13} />
                New folder
              </button>
              <div className="spacer" />
              <button className="btn" onClick={onClose}>
                Cancel
              </button>
              <button className="btn btn--primary" onClick={() => onPick(here)}>
                Choose
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
