import { useCallback, useEffect, useRef, useState } from "react";
import { Icon, type IconName } from "../lib/icons";

/* --- Segmented control --------------------------------------------------- */

export function Segmented<T extends string>({
  value,
  options,
  onChange,
}: {
  value: T;
  options: { value: T; label: string }[];
  onChange(v: T): void;
}) {
  return (
    <div className="segmented" role="tablist">
      {options.map((o) => (
        <button
          key={o.value}
          role="tab"
          aria-selected={o.value === value}
          className={`segmented__item ${
            o.value === value ? "segmented__item--active" : ""
          }`}
          onClick={() => onChange(o.value)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

/* --- Empty state --------------------------------------------------------- */

export function Empty({
  icon,
  title,
  text,
  action,
}: {
  icon: IconName;
  title: string;
  text: string;
  action?: React.ReactNode;
}) {
  return (
    <div className="empty">
      <div className="empty__icon">
        <Icon name={icon} size={22} />
      </div>
      <div className="empty__title">{title}</div>
      <div className="empty__text">{text}</div>
      {action && <div style={{ marginTop: "var(--sp-4)" }}>{action}</div>}
    </div>
  );
}

/* --- Switch / checkbox --------------------------------------------------- */

export function Switch({
  on,
  onChange,
}: {
  on: boolean;
  onChange(v: boolean): void;
}) {
  return (
    <button
      role="switch"
      aria-checked={on}
      className={`switch ${on ? "switch--on" : ""}`}
      onClick={() => onChange(!on)}
    >
      <span className="switch__knob" />
    </button>
  );
}

export function Checkbox({
  on,
  onChange,
}: {
  on: boolean;
  onChange(v: boolean): void;
}) {
  return (
    <button
      role="checkbox"
      aria-checked={on}
      className={`checkbox ${on ? "checkbox--on" : ""}`}
      onClick={() => onChange(!on)}
    >
      <Icon name="check" size={11} strokeWidth={2.4} />
    </button>
  );
}

/* --- Select (native-looking, non-functional in v1) ----------------------- */

export function Select({
  value,
  small,
}: {
  value: string;
  small?: boolean;
}) {
  return (
    <button className={`select ${small ? "select--sm" : ""}`}>
      {value}
      <span className="select__chevron">
        <Icon name="chevronUD" size={12} />
      </span>
    </button>
  );
}

/* --- Draggable splitter -------------------------------------------------- */

/**
 * Resizes a sibling pane by writing a CSS variable on :root, so the pane width
 * survives re-renders and both the flex-basis and width stay in sync.
 */
export function Splitter({
  variable,
  min,
  max,
  invert = false,
}: {
  variable: string;
  min: number;
  max: number;
  invert?: boolean;
}) {
  const [dragging, setDragging] = useState(false);
  const startX = useRef(0);
  const startW = useRef(0);

  const onDown = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      startX.current = e.clientX;
      startW.current = parseFloat(
        getComputedStyle(document.documentElement).getPropertyValue(variable)
      );
      setDragging(true);
    },
    [variable]
  );

  useEffect(() => {
    if (!dragging) return;

    const onMove = (e: MouseEvent) => {
      const delta = (e.clientX - startX.current) * (invert ? -1 : 1);
      const next = Math.min(max, Math.max(min, startW.current + delta));
      document.documentElement.style.setProperty(variable, `${next}px`);
    };
    const onUp = () => setDragging(false);

    document.body.style.cursor = "col-resize";
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      document.body.style.cursor = "";
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, [dragging, invert, max, min, variable]);

  return (
    <div
      className={`splitter ${dragging ? "splitter--dragging" : ""}`}
      onMouseDown={onDown}
      role="separator"
      aria-orientation="vertical"
    />
  );
}

/* --- Toolbar search ------------------------------------------------------ */

export function Search({
  value,
  onChange,
  placeholder = "Search",
}: {
  value: string;
  onChange(v: string): void;
  placeholder?: string;
}) {
  return (
    <label className="search" style={{ flex: 1 }}>
      <Icon name="search" size={13} />
      <input
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        spellCheck={false}
      />
      {value && (
        <button
          className="iconbtn"
          style={{ width: 16, height: 16 }}
          onClick={() => onChange("")}
        >
          <Icon name="close" size={11} />
        </button>
      )}
    </label>
  );
}

/* --- Inspector primitives ------------------------------------------------ */

export function InspGroup({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div className="insp-group">
      <div className="insp-group__title">{title}</div>
      {children}
    </div>
  );
}

export function InspRow({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="insp-row">
      <div className="insp-row__label">{label}</div>
      <div className="insp-row__value">{children}</div>
    </div>
  );
}

/* --- Form primitives ----------------------------------------------------- */

export function FormRow({
  label,
  help,
  children,
}: {
  label: string;
  help?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="formrow">
      <div className="formrow__label">{label}</div>
      <div className="formrow__control">
        {children}
        {help && <div className="formrow__help">{help}</div>}
      </div>
    </div>
  );
}
