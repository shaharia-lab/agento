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
  disabled,
}: {
  on: boolean;
  onChange(v: boolean): void;
  /**
   * Refuses the click *and* says so to assistive tech — `aria-disabled` alone
   * would leave a switch that reads as unavailable and still toggles.
   *
   * It carries no `title`, and that is deliberate rather than an omission: a
   * `disabled` button receives no mouse events, so a tooltip on one never
   * shows. A switch that is shut has to say why in something beside it.
   */
  disabled?: boolean;
}) {
  return (
    <button
      role="switch"
      aria-checked={on}
      disabled={disabled}
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

/* --- Context menu --------------------------------------------------------- */

export interface ContextMenuItem {
  label: string;
  icon?: IconName;
  danger?: boolean;
  disabled?: boolean;
  onSelect(): void;
}

/**
 * A menu anchored to a **point** rather than to a control — what a right-click
 * on a row opens.
 *
 * It is written beside `Dropdown` rather than by extracting a shared popover
 * out of it. `Dropdown` is a working, keyboard-accessible listbox used across
 * several views, and refactoring its internals is a larger and riskier change
 * than the one consumer here justifies; extract the popover if a third one
 * appears.
 *
 * Two things differ from `Dropdown`, and both follow from there being no
 * trigger element:
 *
 * - **Placement is measured, not estimated.** `Dropdown` clamps against its
 *   trigger's rect, which it has before the menu exists. Here the only input is
 *   a pointer position, so the first pass renders at the origin, *hidden*, and
 *   a layout effect clamps against the box the browser actually laid out —
 *   before it paints, so there is no flash. Rendering that first pass at `at`
 *   instead would measure a shrink-to-fit box squeezed by whatever room is left
 *   to the right of the cursor, i.e. the wrong width near the edge this exists
 *   to handle.
 * - **Keys are taken from `window` in the capture phase, not from an
 *   `onKeyDown` prop.** `Dropdown` can use the prop because its handler sits on
 *   the trigger, which is inside the React root; this menu is portaled to
 *   `<body>`, and a `keydown` there does not reach React's delegated listener —
 *   measured in the running app, where Escape and the arrow keys did nothing
 *   at all. Capturing at the window also puts this ahead of the sessions
 *   list's own global Enter handler, so `stopPropagation` on a key the menu
 *   claims is what stops Enter opening the session *and* picking the item. The
 *   menu still takes focus on open, which is where a menu's focus belongs.
 */
export function ContextMenu({
  at,
  items,
  onClose,
}: {
  /** Viewport coordinates — `clientX` / `clientY` of the opening event. */
  at: { x: number; y: number };
  items: ContextMenuItem[];
  onClose(): void;
}) {
  const menuRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number }>();
  const [active, setActive] = useState(-1);

  useLayoutEffect(() => {
    const el = menuRef.current;
    if (!el) return;
    const { width, height } = el.getBoundingClientRect();
    const m = 4;
    setPos({
      left: Math.max(m, Math.min(at.x, window.innerWidth - width - m)),
      top: Math.max(m, Math.min(at.y, window.innerHeight - height - m)),
    });
  }, [at.x, at.y]);

  // Focus only once `pos` has made the menu visible: a `visibility: hidden`
  // element is not focusable, so focusing in the measuring effect above is
  // silently a no-op — which is exactly what it did until the running app was
  // asked what `document.activeElement` was.
  useEffect(() => {
    if (pos) menuRef.current?.focus({ preventScroll: true });
  }, [pos]);

  // "Outside" is outside the menu alone: the point it was opened at belongs to
  // whatever the user right-clicked, which must not swallow the dismissal.
  // Any scroll or resize under an open menu would strand it, so close.
  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (menuRef.current?.contains(e.target as Node)) return;
      onClose();
    };
    const onScroll = (e: Event) => {
      if (menuRef.current?.contains(e.target as Node)) return;
      onClose();
    };
    document.addEventListener("mousedown", onDown);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onClose);
    window.addEventListener("blur", onClose);
    return () => {
      document.removeEventListener("mousedown", onDown);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onClose);
      window.removeEventListener("blur", onClose);
    };
  }, [onClose]);

  /** Walk to the next enabled item, wrapping; a menu of only disabled items stays put. */
  const move = useCallback(
    (dir: 1 | -1) => {
      setActive((i) => {
        const n = items.length;
        if (n === 0) return -1;
        let next = i < 0 && dir === 1 ? -1 : Math.max(0, i);
        for (let k = 0; k < n; k++) {
          next = (next + dir + n) % n;
          if (!items[next].disabled) return next;
        }
        return i;
      });
    },
    [items]
  );

  // Close first, then act: an item may navigate away and unmount this tree.
  const fire = useCallback(
    (i: number) => {
      const item = items[i];
      if (!item || item.disabled) return;
      onClose();
      item.onSelect();
    },
    [items, onClose]
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      switch (e.key) {
        case "Escape":
        case "Tab":
          onClose();
          break;
        case "ArrowDown":
          move(1);
          break;
        case "ArrowUp":
          move(-1);
          break;
        case "Home":
          setActive(-1);
          move(1);
          break;
        case "End":
          setActive(-1);
          move(-1);
          break;
        case "Enter":
        case " ":
          if (active < 0) return;
          fire(active);
          break;
        default:
          return;
      }
      // Only the keys the menu claims are swallowed, and only while it is open.
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [active, move, fire, onClose]);

  return createPortal(
    <div
      ref={menuRef}
      className="menu ctxmenu"
      role="menu"
      tabIndex={-1}
      style={
        pos
          ? { left: pos.left, top: pos.top }
          : { left: 0, top: 0, visibility: "hidden" }
      }
    >
      {items.map((item, i) => (
        <button
          key={item.label}
          type="button"
          role="menuitem"
          data-index={i}
          disabled={item.disabled}
          className={`menu__item ${item.danger ? "menu__item--danger" : ""} ${
            i === active ? "menu__item--active" : ""
          }`}
          onMouseEnter={() => setActive(i)}
          onClick={() => fire(i)}
        >
          <span className="ctxmenu__icon">
            {item.icon && <Icon name={item.icon} size={13} />}
          </span>
          <span className="truncate">{item.label}</span>
        </button>
      ))}
    </div>,
    document.body
  );
}

/* --- Combobox ------------------------------------------------------------ */

/**
 * A text field with a filtered list of suggestions — an **open** set, where
 * `Dropdown` above is a closed one.
 *
 * The distinction is the whole reason this exists rather than a `Dropdown` with
 * an extra prop. A `Dropdown`'s value must be one of its options; here the
 * options are only a *catalog*, and a value that is not in the list is a
 * first-class answer — a model released this morning, a fine-tune id, a
 * provider whose list endpoint this build could not reach. So the value lives in
 * the input, `onChange` fires on every keystroke exactly as the plain `<input>`
 * this replaces did, and picking a suggestion is a shortcut rather than the only
 * route. Nothing here can reject what the user typed.
 *
 * With no options it is deliberately indistinguishable from that plain input:
 * the menu never opens and no chevron is drawn, which is what lets a caller
 * degrade to free text by passing an empty list rather than by branching.
 */
export function Combobox({
  value,
  options,
  onChange,
  placeholder,
  className,
  disabled,
  ariaLabel,
}: {
  value: string;
  /** Suggestions. Empty means "behave as a plain text field". */
  options: string[];
  onChange(v: string): void;
  placeholder?: string;
  className?: string;
  disabled?: boolean;
  ariaLabel?: string;
}) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(-1);
  const wrapRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{
    left: number;
    top: number;
    minWidth: number;
    maxHeight: number;
    up: boolean;
  }>();

  // Filtered by what is typed, case-insensitively and as a substring rather
  // than a prefix — model ids are versioned and vendor-prefixed, so the part a
  // user remembers ("sonnet", "4o") is rarely at the front.
  const needle = value.trim().toLowerCase();
  const matches = needle
    ? options.filter((o) => o.toLowerCase().includes(needle))
    : options;

  const close = useCallback(() => setOpen(false), []);

  const openMenu = useCallback(() => {
    const el = wrapRef.current;
    if (!el || options.length === 0) return;
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
    setActive(-1);
    setOpen(true);
  }, [options.length]);

  // Same portal rules as Dropdown: "outside" spans the trigger and the menu,
  // and any scroll or resize underneath would strand the menu.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      const t = e.target as Node;
      if (menuRef.current?.contains(t) || wrapRef.current?.contains(t)) return;
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

  // A filter that empties the list leaves an open menu with nothing in it, and
  // an active index pointing past the end.
  useEffect(() => {
    if (open && matches.length === 0) close();
    setActive((i) => (i >= matches.length ? matches.length - 1 : i));
  }, [matches.length, open, close]);

  useEffect(() => {
    if (!open || active < 0) return;
    menuRef.current
      ?.querySelector(`[data-index="${active}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, [open, active]);

  function pick(v: string) {
    setOpen(false);
    onChange(v);
    inputRef.current?.focus();
  }

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      // Only swallow Escape when there is a menu to close, so it still reaches
      // whatever the field sits inside.
      if (open) {
        e.preventDefault();
        close();
      }
      return;
    }
    if (!open) {
      if (e.key === "ArrowDown" && matches.length > 0) {
        e.preventDefault();
        openMenu();
      }
      return;
    }
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        setActive((i) => Math.min(matches.length - 1, i + 1));
        break;
      case "ArrowUp":
        e.preventDefault();
        setActive((i) => Math.max(0, i - 1));
        break;
      case "Enter":
        // Enter commits the highlighted suggestion, and otherwise does
        // nothing but close — what is typed is already the value, so
        // "confirming" it must never rewrite it to the nearest match.
        if (active >= 0 && matches[active]) {
          e.preventDefault();
          pick(matches[active]);
        } else {
          close();
        }
        break;
      case "Tab":
        close();
        break;
    }
  };

  return (
    <>
      <div
        ref={wrapRef}
        className={`combo ${options.length > 0 ? "combo--suggesting" : ""} ${
          className ?? ""
        }`}
      >
        <input
          ref={inputRef}
          className="field mono combo__input"
          value={value}
          placeholder={placeholder}
          disabled={disabled}
          aria-label={ariaLabel}
          role="combobox"
          aria-expanded={open}
          aria-autocomplete="list"
          onChange={(e) => {
            onChange(e.target.value);
            if (!open) openMenu();
          }}
          onFocus={openMenu}
          onKeyDown={onKeyDown}
        />
        {options.length > 0 && (
          <button
            type="button"
            className="combo__chevron"
            tabIndex={-1}
            disabled={disabled}
            aria-label="Show model ids"
            onClick={() => (open ? close() : (inputRef.current?.focus(), openMenu()))}
          >
            <Icon name="chevronUD" size={12} />
          </button>
        )}
      </div>

      {open &&
        pos &&
        matches.length > 0 &&
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
            {matches.map((o, i) => (
              <button
                key={o}
                type="button"
                role="option"
                data-index={i}
                aria-selected={o === value}
                className={`dropdown__item mono ${
                  i === active ? "dropdown__item--active" : ""
                }`}
                onMouseEnter={() => setActive(i)}
                // `mousedown` rather than `click`: the input's blur would
                // otherwise close the menu and unmount the row before the click
                // landed on it.
                onMouseDown={(e) => {
                  e.preventDefault();
                  pick(o);
                }}
              >
                <span className="dropdown__check">
                  {o === value && <Icon name="check" size={11} />}
                </span>
                <span className="truncate">{o}</span>
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
