/**
 * The export options for one session (#591) — format, scope, Save-As.
 *
 * One component for every entry point: the list's right-click menu,
 * `SessionLink`'s, and the detail page's toolbar button all open this same
 * panel, so the three cannot drift into different defaults.
 *
 * It is a **non-modal popover anchored to a point**, the way `ContextMenu` is,
 * rather than a dialog: the repo has no modal primitive, and the menu that
 * opens it already names a point on screen to open at. It closes on Escape and
 * on a click outside it — **not on window blur**, unlike the menu, because the
 * native Save-As dialog it opens takes focus from the window.
 */
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { describeError } from "../../lib/hooks";
import { Icon } from "../../lib/icons";
import {
  DEFAULT_EXPORT_OPTIONS,
  EXPORT_AVAILABLE,
  exportFileName,
  exportSession,
  pickExportPath,
  type ExportFormat,
  type ExportOptions,
} from "../../lib/sessionExport";
import { Checkbox, Segmented } from "../../components/ui";
import "../../styles/sessionexport.css";

/** What an entry point hands over: which session, and where to open. */
export interface SessionExportTarget {
  sessionId: string;
  /** The session's display title, for the default file name. */
  title?: string;
  at: { x: number; y: number };
}

const FORMATS: { value: ExportFormat; label: string }[] = [
  { value: "markdown", label: "Markdown" },
  { value: "jsonl", label: "JSONL" },
  { value: "text", label: "Plain text" },
];

type Toggle = Exclude<keyof ExportOptions, "format">;

const TOGGLES: { key: Toggle; label: string }[] = [
  { key: "include_reasoning", label: "Reasoning / thinking blocks" },
  { key: "include_tool_calls", label: "Tool calls (name + input)" },
  { key: "include_tool_results", label: "Tool results and attachments" },
  { key: "include_system", label: "System messages" },
  { key: "include_metadata", label: "Metadata header" },
];

export function SessionExportPanel({
  target,
  onClose,
}: {
  target: SessionExportTarget;
  onClose(): void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number }>();
  const [opts, setOpts] = useState<ExportOptions>(DEFAULT_EXPORT_OPTIONS);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState<string>();
  const [error, setError] = useState<string>();

  // A new target is a new export: nothing carries over from the last session.
  useEffect(() => {
    setOpts(DEFAULT_EXPORT_OPTIONS);
    setSaved(undefined);
    setError(undefined);
  }, [target.sessionId]);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const { width, height } = el.getBoundingClientRect();
    const m = 4;
    setPos({
      left: Math.max(m, Math.min(target.at.x, window.innerWidth - width - m)),
      top: Math.max(m, Math.min(target.at.y, window.innerHeight - height - m)),
    });
  }, [target.at.x, target.at.y]);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (saving || ref.current?.contains(e.target as Node)) return;
      onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !saving) onClose();
    };
    document.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [onClose, saving]);

  async function run() {
    setError(undefined);
    setSaved(undefined);
    setSaving(true);
    try {
      const dest = await pickExportPath(
        exportFileName(target.title, target.sessionId, opts.format),
        opts.format
      );
      if (!dest) return;
      const res = await exportSession(target.sessionId, dest, opts);
      setSaved(
        res.attachments > 0
          ? `Saved to ${res.path}, with ${res.attachments} ${
              res.attachments === 1 ? "attachment" : "attachments"
            } in ${res.attachments_dir}`
          : `Saved to ${res.path}`
      );
    } catch (err) {
      setError(describeError(err));
    } finally {
      setSaving(false);
    }
  }

  return createPortal(
    <div
      ref={ref}
      className="menu sess-export"
      role="dialog"
      aria-label="Export session"
      style={
        pos
          ? { left: pos.left, top: pos.top }
          : { left: 0, top: 0, visibility: "hidden" }
      }
    >
      <div className="sess-export__title">Export session</div>
      <div className="sess-export__label">Format</div>
      <Segmented<ExportFormat>
        value={opts.format}
        options={FORMATS}
        onChange={(format) => setOpts((o) => ({ ...o, format }))}
      />
      <div className="sess-export__label">Include</div>
      {TOGGLES.map((t) => (
        <label key={t.key} className="sess-export__toggle">
          <Checkbox
            on={opts[t.key]}
            onChange={(on) => setOpts((o) => ({ ...o, [t.key]: on }))}
          />
          <span>{t.label}</span>
        </label>
      ))}
      {!EXPORT_AVAILABLE && (
        <div className="sess-export__note">
          Exporting needs the desktop app.
        </div>
      )}
      {saved && <div className="sess-export__note">{saved}</div>}
      {error && <div className="sess-export__error">{error}</div>}
      <div className="sess-export__actions">
        <button type="button" className="btn" onClick={onClose} disabled={saving}>
          {saved ? "Close" : "Cancel"}
        </button>
        <button
          type="button"
          className="btn btn--primary"
          onClick={run}
          disabled={saving || !EXPORT_AVAILABLE}
        >
          <Icon name="download" size={13} />
          {saving ? "Exporting…" : "Export…"}
        </button>
      </div>
    </div>,
    document.body
  );
}
