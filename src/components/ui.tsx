import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
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

/* --- Dropdown ------------------------------------------------------------- */

export interface DropdownOption {
  value: string;
  label: string;
}

/**
 * The app's one select control. A native <select> keeps the platform's own
 * popup, which fights the app styling on every OS (worst on WebKitGTK), so the
 * menu is rendered by the app instead: a portal to <body>, positioned off the
 * trigger, styled like every other .menu. Replaces both the old display-only
 * Select and the per-view native-<select> wrappers.
 */
export function Dropdown({
  value,
  options,
  onChange,
  small,
  disabled,
  label,
  placeholder = "Choose…",
  className,
  style,
  ariaLabel,
}: {
  value: string;
  options: DropdownOption[];
  onChange(v: string): void;
  small?: boolean;
  disabled?: boolean;
  /** Overrides the trigger text (e.g. "Sort: Recent"). */
  label?: string;
  placeholder?: string;
  className?: string;
  style?: React.CSSProperties;
  ariaLabel?: string;
}) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(-1);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{
    left: number;
    top: number;
    minWidth: number;
    maxHeight: number;
    up: boolean;
  }>();

  const selectedIndex = options.findIndex((o) => o.value === value);
  const current = selectedIndex >= 0 ? options[selectedIndex] : undefined;

  const openMenu = useCallback(() => {
    const el = triggerRef.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const below = window.innerHeight - r.bottom - 8;
    const above = r.top - 8;
    const up = below < 160 && above > below;
    setPos({
      left: Math.max(4, Math.min(r.left, window.innerWidth - r.width - 4)),
      top: up ? r.top - 4 : r.bottom + 4,
      minWidth: r.width,
      maxHeight: Math.min(320, up ? above : below),
      up,
    });
    setActive(options.findIndex((o) => o.value === value));
    setOpen(true);
  }, [options, value]);

  const close = useCallback(() => setOpen(false), []);

  const pick = useCallback(
    (v: string) => {
      setOpen(false);
      if (v !== value) onChange(v);
    },
    [onChange, value]
  );

  // The menu is a portal, so "outside" means outside both the trigger and the
  // menu. Any scroll or resize under an open menu would strand it, so close.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      const t = e.target as Node;
      if (menuRef.current?.contains(t) || triggerRef.current?.contains(t)) return;
      close();
    };
    const onScroll = (e: Event) => {
      if (menuRef.current?.contains(e.target as Node)) return;
      close();
    };
    document.addEventListener("mousedown", onDown);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", close);
    return () => {
      document.removeEventListener("mousedown", onDown);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", close);
    };
  }, [open, close]);

  // Keep the selected row in view when the menu opens.
  useLayoutEffect(() => {
    if (!open) return;
    menuRef.current
      ?.querySelector('[aria-selected="true"]')
      ?.scrollIntoView({ block: "nearest" });
  }, [open]);

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (!open) {
      if (["ArrowDown", "ArrowUp", "Enter", " "].includes(e.key)) {
        e.preventDefault();
        openMenu();
      }
      return;
    }
    switch (e.key) {
      case "Escape":
        e.preventDefault();
        close();
        break;
      case "ArrowDown":
        e.preventDefault();
        setActive((i) => Math.min(options.length - 1, i + 1));
        break;
      case "ArrowUp":
        e.preventDefault();
        setActive((i) => Math.max(0, i < 0 ? 0 : i - 1));
        break;
      case "Home":
        e.preventDefault();
        setActive(0);
        break;
      case "End":
        e.preventDefault();
        setActive(options.length - 1);
        break;
      case "Enter":
      case " ":
        e.preventDefault();
        if (active >= 0 && options[active]) pick(options[active].value);
        else close();
        break;
      case "Tab":
        close();
        break;
    }
  };

  // Keyboard movement must keep the active row visible in a scrolled menu.
  useEffect(() => {
    if (!open || active < 0) return;
    menuRef.current
      ?.querySelector(`[data-index="${active}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, [open, active]);

  return (
    <>
      <button
        type="button"
        ref={triggerRef}
        className={`select ${small ? "select--sm" : ""} ${
          open ? "select--open" : ""
        } ${className ?? ""}`}
        style={style}
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={ariaLabel}
        title={label ?? current?.label}
        onClick={() => (open ? close() : openMenu())}
        onKeyDown={onKeyDown}
      >
        <span className="select__label">
          {label ?? current?.label ?? placeholder}
        </span>
        <span className="select__chevron">
          <Icon name="chevronUD" size={12} />
        </span>
      </button>

      {open &&
        pos &&
        createPortal(
          <div
            ref={menuRef}
            className="menu dropdown__menu scroll"
            role="listbox"
            style={{
              left: pos.left,
              ...(pos.up
                ? { bottom: window.innerHeight - pos.top }
                : { top: pos.top }),
              minWidth: pos.minWidth,
              maxHeight: pos.maxHeight,
            }}
          >
            {options.map((o, i) => (
              <button
                key={o.value}
                type="button"
                role="option"
                data-index={i}
                aria-selected={o.value === value}
                className={`dropdown__item ${
                  i === active ? "dropdown__item--active" : ""
                }`}
                onMouseEnter={() => setActive(i)}
                onClick={() => pick(o.value)}
              >
                <span className="dropdown__check">
                  {o.value === value && <Icon name="check" size={11} />}
                </span>
                <span className="truncate">{o.label}</span>
              </button>
            ))}
          </div>,
          document.body
        )}
    </>
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
