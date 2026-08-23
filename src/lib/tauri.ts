/* ============================================================================
   Tauri bridge — every call degrades gracefully so the UI also runs in a plain
   browser tab (`npm run dev`) for fast iteration on layout.
   ========================================================================== */

export const IS_TAURI =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** Platform facts the Rust side resolves at startup. */
export interface HostInfo {
  os: string;
  arch: string;
  version: string;
  controls_on_left: boolean;
  api_base: string;
  /** Path to the Claude Code CLI, or null when it is not installed. */
  claude_cli: string | null;
  /** Whether this install can replace itself, or only announce updates. */
  can_self_update: boolean;
  /** "appimage" | "package" | "dmg" | "installer" */
  install_kind: string;
  /** This launch's bearer token for /api. Empty outside Tauri. */
  api_token: string;
}

let hostInfoPromise: Promise<HostInfo | null> | undefined;

/**
 * The `host_info` command, fetched at most once per page.
 *
 * Memoized here rather than in `host.ts` because `api.ts` needs it too and must
 * not depend on React — and because two independent callers invoking the same
 * command is two round trips for one immutable answer.
 *
 * **This is how the /api bearer token reaches the page (#400).** IPC is the one
 * channel a local process cannot reach, which is the entire point: a token
 * delivered over `/api` itself would be a token anything could ask for. That
 * makes this call load-bearing where it used to be advisory, so a failure
 * resolves to `null` rather than rejecting — every caller already has to handle
 * "outside Tauri", and collapsing the two cases means one path to get right.
 */
export function hostInfo(): Promise<HostInfo | null> {
  if (!hostInfoPromise) {
    hostInfoPromise = !IS_TAURI
      ? Promise.resolve(null)
      : import("@tauri-apps/api/core")
          .then(({ invoke }) => invoke<HostInfo>("host_info"))
          .catch(() => null);
  }
  return hostInfoPromise;
}

/**
 * Drop the memoized answer so the next `hostInfo()` invokes the command again.
 *
 * **The token is no longer immutable for the life of the process (#405).** It is
 * a JWT signed by the install's keypair, minted fresh on every `host_info`
 * invocation, and it has two ways to stop working: its `exp` passing, and the
 * user regenerating the keypair from Settings → Security — which is *meant* to
 * sign this window out, since "invalidate everything, now" that spared the app
 * itself would not be invalidating everything.
 *
 * So the memo has to be droppable, and `api.ts` drops it on a 401. Exported only
 * for that one caller: nothing else has a reason to re-ask, and a view calling
 * this on a whim would turn one IPC round trip per page into one per render.
 */
export function resetHostInfo(): void {
  hostInfoPromise = undefined;
}

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
