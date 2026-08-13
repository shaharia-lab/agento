import { useEffect, useState } from "react";
import { Icon } from "../lib/icons";
import {
  IS_MAC,
  MOD,
  winClose,
  winIsMaximized,
  winMinimize,
  winToggleMaximize,
} from "../lib/tauri";

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
 * Unified titlebar: window drag region, navigation, and window controls in one
 * strip. Decorations are off in tauri.conf.json, so this is the real chrome.
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
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    winIsMaximized().then(setMaximized);
  }, []);

  const toggleMax = () => {
    winToggleMaximize();
    setMaximized((m) => !m);
  };

  return (
    <div className="titlebar">
      {/* macOS draws its own traffic lights on the left; reserve room for them. */}
      {IS_MAC && <div style={{ width: 68 }} />}

      <div className="row" style={{ gap: 2 }}>
        <button
          className={`iconbtn ${sidebarOpen ? "" : "iconbtn--active"}`}
          onClick={onToggleSidebar}
          title={`Toggle Sidebar  ${MOD} B`}
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

      <div className="row no-drag" style={{ marginLeft: "auto", gap: 2 }}>
        <button
          className="iconbtn"
          onClick={onOpenPalette}
          title={`Command Palette  ${MOD} K`}
        >
          <Icon name="command" />
        </button>

        {!IS_MAC && (
          <div className="wincontrols" style={{ marginLeft: 6 }}>
            <button className="wincontrol" onClick={winMinimize} title="Minimize">
              <Icon name="minus" size={14} />
            </button>
            <button
              className="wincontrol"
              onClick={toggleMax}
              title={maximized ? "Restore" : "Maximize"}
            >
              <svg
                width="12"
                height="12"
                viewBox="0 0 12 12"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.3"
              >
                {maximized ? (
                  <>
                    <rect x="1.5" y="3.5" width="6" height="6" rx="1" />
                    <path d="M4 3.5V2.5a1 1 0 0 1 1-1h4.5a1 1 0 0 1 1 1V7a1 1 0 0 1-1 1H8.5" />
                  </>
                ) : (
                  <rect x="2" y="2" width="8" height="8" rx="1.2" />
                )}
              </svg>
            </button>
            <button
              className="wincontrol wincontrol--close"
              onClick={winClose}
              title="Close"
            >
              <Icon name="close" size={14} />
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
