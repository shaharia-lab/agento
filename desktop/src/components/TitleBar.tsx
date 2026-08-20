import { Icon } from "../lib/icons";
import { IS_MAC, IS_TAURI, MOD } from "../lib/tauri";

interface Props {
  title: string;
  subtitle?: string;
  sidebarOpen: boolean;
  onToggleSidebar(): void;
  onOpenPalette(): void;
  onBack(): void;
  onForward(): void;
  canBack: boolean;
  canForward: boolean;
}

/**
 * Unified titlebar: window drag region plus navigation in one strip. The OS
 * draws the window controls — macOS inset traffic lights over this strip
 * (titleBarStyle "Overlay"), Linux/Windows their own decorated titlebar above
 * it — so nothing here re-implements minimize/maximize/close.
 *
 * Dragging is Tauri's `data-tauri-drag-region`, declared **once, as `deep`**,
 * on the outer strip. A bare attribute means "only a direct click on this
 * element drags" (`el === composedPath[0]` in Tauri's own `drag.js`), so it had
 * to be repeated on every container and still missed anything nested one level
 * further — the `<small>` in the title, an icon inside a non-button wrapper.
 * `deep` walks the composed path instead, and the walk stops at the first
 * clickable ancestor (`A`, `BUTTON`, `INPUT`, `[role=button]`, …) that does not
 * carry the attribute itself, so every button here still blocks the drag
 * without opting out. Repeating a *bare* attribute on an inner container would
 * now defeat this: the walk hits it first and answers "not a direct click".
 *
 * None of this works at all unless the window's origin is in scope for the ACL
 * — see the `remote` block in `src-tauri/capabilities/default.json` and the
 * comment on the release navigation in `src-tauri/src/lib.rs`.
 */
export function TitleBar({
  title,
  subtitle,
  sidebarOpen,
  onToggleSidebar,
  onOpenPalette,
  onBack,
  onForward,
  canBack,
  canForward,
}: Props) {
  return (
    <div className="titlebar" data-tauri-drag-region="deep">
      {/* macOS draws its traffic lights over the left edge; reserve room.
          The shell places them at x=16 with AppKit's ~20px button pitch
          (src-tauri/src/macos_window.rs), so the group ends near 70px; with
          the strip's own 8px padding this starts the icons at ~84px. */}
      {IS_TAURI && IS_MAC && <div style={{ width: 76, flex: "0 0 auto" }} />}

      <div className="row" style={{ gap: 2 }}>
        <button
          className={`iconbtn ${sidebarOpen ? "" : "iconbtn--active"}`}
          onClick={onToggleSidebar}
          title={`Toggle Sidebar  ${MOD} B`}
        >
          <Icon name="sidebar" />
        </button>

        <div className="toolbar__sep" />

        <button
          className="iconbtn"
          onClick={onBack}
          disabled={!canBack}
          title="Back"
        >
          <Icon name="chevronR" rotate={180} />
        </button>
        <button
          className="iconbtn"
          onClick={onForward}
          disabled={!canForward}
          title="Forward"
        >
          <Icon name="chevronR" />
        </button>
      </div>

      <div className="titlebar__title">
        {title}
        {subtitle && <small>{"  —  " + subtitle}</small>}
      </div>

      <div className="row" style={{ marginLeft: "auto", gap: 2 }}>
        <button
          className="iconbtn"
          onClick={onOpenPalette}
          title={`Command Palette  ${MOD} K`}
        >
          <Icon name="command" />
        </button>
      </div>
    </div>
  );
}
