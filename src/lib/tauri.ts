/* ============================================================================
   Tauri bridge — every call degrades gracefully so the UI also runs in a plain
   browser tab (`npm run dev`) for fast iteration on layout.
   ========================================================================== */

export const IS_TAURI =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

type AppWindow = {
  setTheme(theme: "light" | "dark" | null): Promise<void>;
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

/**
 * Native folder picker for path fields — writing an absolute path by hand is
 * the kind of thing a web app forces and a desktop app must not. Returns the
 * chosen directory, or null when cancelled or when running in a plain browser
 * tab, where the caller keeps its text input as the fallback.
 */
export async function pickDirectory(
  title: string,
  defaultPath?: string
): Promise<string | null> {
  if (!IS_TAURI) return null;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({
    directory: true,
    multiple: false,
    title,
    defaultPath: defaultPath || undefined,
  });
  return typeof picked === "string" ? picked : null;
}

/**
 * Open a URL in the user's real browser. `window.open`/`target="_blank"` do
 * not reliably leave a Tauri webview (WKWebView ignores them, WebKitGTK needs
 * a create handler), so every external link must go through the opener
 * plugin. The plugin being unavailable falls back rather than failing.
 */
export async function openExternal(url: string): Promise<void> {
  if (IS_TAURI) {
    try {
      const { openUrl } = await import("@tauri-apps/plugin-opener");
      await openUrl(url);
      return;
    } catch (err) {
      console.warn("opener plugin unavailable, falling back to window.open", err);
    }
  }
  window.open(url, "_blank", "noopener,noreferrer");
}

/**
 * Mirror the user's explicit appearance choice onto the native window, so the
 * OS-drawn titlebar and UA surfaces follow the app instead of the OS. `null`
 * returns the window to following the system.
 */
export async function setWindowTheme(theme: "light" | "dark" | null) {
  (await appWindow())?.setTheme(theme);
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
