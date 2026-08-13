import { Icon } from "../lib/icons";

interface Props {
  running: number;
  connected: boolean;
  model: string;
  tokensToday: string;
  costToday: string;
  inspectorOpen: boolean;
  onToggleInspector(): void;
  theme: "light" | "dark" | "system";
  onCycleTheme(): void;
}

/**
 * Status bar — the persistent, always-truthful strip along the bottom.
 * Web apps almost never have one; desktop apps almost always do.
 */
export function StatusBar({
  running,
  connected,
  model,
  tokensToday,
  costToday,
  inspectorOpen,
  onToggleInspector,
  theme,
  onCycleTheme,
}: Props) {
  return (
    <div className="statusbar">
      <div className="statusbar__item">
        <span
          className={`dot ${running > 0 ? "dot--green dot--pulse" : "dot--idle"}`}
        />
        {running > 0
          ? `${running} job${running > 1 ? "s" : ""} running`
          : "Idle"}
      </div>

      <div className="statusbar__item">
        <Icon name="cpu" size={12} />
        {model}
      </div>

      <div className="statusbar__item">
        <Icon name="zap" size={12} />
        <span className="tnum">{tokensToday}</span> today
      </div>

      <div className="statusbar__item">
        <Icon name="dollar" size={12} />
        <span className="tnum">{costToday}</span>
      </div>

      <div className="spacer" />

      <button
        className="statusbar__item statusbar__item--button"
        onClick={onCycleTheme}
        title="Switch appearance"
      >
        <Icon name="palette" size={12} />
        {theme === "system" ? "Auto" : theme === "dark" ? "Dark" : "Light"}
      </button>

      <div className="statusbar__item">
        <span className={`dot ${connected ? "dot--green" : "dot--red"}`} />
        {connected ? "Connected" : "Backend unreachable"}
      </div>

      <button
        className={`statusbar__item statusbar__item--button ${
          inspectorOpen ? "" : ""
        }`}
        onClick={onToggleInspector}
        title="Toggle Inspector"
      >
        <Icon name="inspector" size={12} />
      </button>
    </div>
  );
}
