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
 * Dragging is Tauri's `data-tauri-drag-region`, which only applies to the
 * element it is set on directly — hence it is repeated on every container
 * whose bare surface should drag. Buttons are separate targets, so they stay
 * clickable without opting out.
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
    <div className="titlebar" data-tauri-drag-region>
      {/* macOS draws its traffic lights over the left edge; reserve room.
          The shell places them at x=16 with AppKit's ~20px button pitch
          (src-tauri/src/macos_window.rs), so the group ends near 70px; with
          the strip's own 8px padding this starts the icons at ~84px. */}
      {IS_TAURI && IS_MAC && (
        <div style={{ width: 76, flex: "0 0 auto" }} data-tauri-drag-region />
      )}

      <div className="row" style={{ gap: 2 }} data-tauri-drag-region>
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

      <div className="titlebar__title" data-tauri-drag-region>
        {title}
        {subtitle && <small>{"  —  " + subtitle}</small>}
      </div>

      <div
        className="row"
        style={{ marginLeft: "auto", gap: 2 }}
        data-tauri-drag-region
      >
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
