import { Icon, type IconName } from "../lib/icons";
import type { ViewId } from "../lib/nav";
import { SECTIONS } from "../lib/nav";
import { MOD } from "../lib/tauri";

interface Props {
  open: boolean;
  active: ViewId;
  onSelect(id: ViewId): void;
  counts: Partial<Record<ViewId, number>>;
  onNewChat(): void;
}

/**
 * Source list. Grouped, captioned, and flush against the window background —
 * the pane the content sits on top of, not a nav bar beside it.
 */
export function Sidebar({ open, active, onSelect, counts, onNewChat }: Props) {
  return (
    <nav className={`sidebar ${open ? "" : "sidebar--hidden"}`}>
      <div style={{ padding: "var(--sp-2) 0 var(--sp-4)" }}>
        <button
          className="btn btn--primary"
          style={{ width: "100%", height: 28 }}
          onClick={onNewChat}
          title={`New Chat  ${MOD} N`}
        >
          <Icon name="plus" size={14} />
          New Chat
        </button>
      </div>

      <div className="sidebar__scroll scroll">
        {SECTIONS.map((section) => (
          <div key={section.caption ?? "root"}>
            {section.caption && (
              <div className="sidebar__caption">{section.caption}</div>
            )}
            {section.items.map((item) => (
              <SourceItem
                key={item.id}
                id={item.id}
                icon={item.icon}
                label={item.label}
                badge={item.badge}
                count={counts[item.id]}
                active={active === item.id}
                onSelect={onSelect}
              />
            ))}
          </div>
        ))}
      </div>

      <div style={{ paddingTop: "var(--sp-3)" }}>
        <div className="divider" style={{ margin: "0 0 var(--sp-3)" }} />
        <SourceItem
          id="settings"
          icon="gear"
          label="Settings"
          active={active === "settings"}
          onSelect={onSelect}
        />
        <SourceItem
          id="about"
          icon="info"
          label="About Agento"
          active={active === "about"}
          onSelect={onSelect}
        />
      </div>
    </nav>
  );
}

function SourceItem({
  id,
  icon,
  label,
  badge,
  count,
  active,
  onSelect,
}: {
  id: ViewId;
  icon: IconName;
  label: string;
  badge?: string;
  count?: number;
  active: boolean;
  onSelect(id: ViewId): void;
}) {
  return (
    <button
      className={`srcitem ${active ? "srcitem--active" : ""}`}
      onClick={() => onSelect(id)}
    >
      <span className="srcitem__icon">
        <Icon name={icon} />
      </span>
      <span className="truncate">{label}</span>
      {badge && (
        <span className="badge badge--amber" style={{ marginLeft: "auto" }}>
          {badge}
        </span>
      )}
      {count !== undefined && !badge && (
        <span className="srcitem__count">{count}</span>
      )}
    </button>
  );
}
