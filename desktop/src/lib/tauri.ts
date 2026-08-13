/* ============================================================================
   Tauri bridge — every call degrades gracefully so the UI also runs in a plain
   browser tab (`npm run dev`) for fast iteration on layout.
   ========================================================================== */

export const IS_TAURI =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

type AppWindow = {
  minimize(): Promise<void>;
  toggleMaximize(): Promise<void>;
  close(): Promise<void>;
  isMaximized(): Promise<boolean>;
  startDragging(): Promise<void>;
  onFocusChanged(
    handler: (event: { payload: boolean }) => void
  ): Promise<() => void>;
};

let cached: AppWindow | null = null;

async function appWindow(): Promise<AppWindow | null> {
  if (!IS_TAURI) return null;
  if (cached) return cached;
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  cached = getCurrentWindow() as unknown as AppWindow;
  return cached;
}

export async function winMinimize() {
  (await appWindow())?.minimize();
}

export async function winToggleMaximize() {
  (await appWindow())?.toggleMaximize();
}

export async function winClose() {
  (await appWindow())?.close();
}

export async function winIsMaximized(): Promise<boolean> {
  const w = await appWindow();
  return w ? w.isMaximized() : false;
}

/**
 * Native apps dim their selection highlight when the window loses focus.
 * Outside Tauri we fall back to the document's own focus events.
 */
export function onWindowFocus(cb: (focused: boolean) => void): () => void {
  if (!IS_TAURI) {
    const on = () => cb(true);
    const off = () => cb(false);
    window.addEventListener("focus", on);
    window.addEventListener("blur", off);
    return () => {
      window.removeEventListener("focus", on);
      window.removeEventListener("blur", off);
    };
  }

  let dispose: (() => void) | undefined;
  let cancelled = false;

  appWindow().then((w) =>
    w?.onFocusChanged(({ payload }) => cb(payload)).then((un) => {
      if (cancelled) un();
      else dispose = un;
    })
  );

  return () => {
    cancelled = true;
    dispose?.();
  };
}

/**
 * Native menu selections arrive here as the menu item's id. The menu carries the
 * accelerators, so this is the same path the keyboard shortcuts take.
 */
export function onMenuAction(cb: (id: string) => void): () => void {
  if (!IS_TAURI) return () => {};

  let dispose: (() => void) | undefined;
  let cancelled = false;

  import("@tauri-apps/api/event").then(({ listen }) =>
    listen<string>("menu://action", (e) => cb(e.payload)).then((un) => {
      if (cancelled) un();
      else dispose = un;
    })
  );

  return () => {
    cancelled = true;
    dispose?.();
  };
}

/** True on macOS, where window controls live on the left and are drawn by the OS. */
export const IS_MAC =
  typeof navigator !== "undefined" &&
  /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);

export const MOD = IS_MAC ? "⌘" : "Ctrl";
