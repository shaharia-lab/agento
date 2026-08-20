/* ============================================================================
   Host facts resolved by the Rust side at startup.
   ========================================================================== */

import { useEffect, useState } from "react";
import { IS_TAURI } from "./tauri";

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
}

/**
 * Everything the installer ships is self-contained except the Claude Code CLI:
 * the backend runs agents by spawning it. Outside Tauri we cannot look at the
 * filesystem, so assume it is present rather than showing a false warning in a
 * browser tab.
 */
export function useHostInfo(): HostInfo | undefined {
  const [info, setInfo] = useState<HostInfo>();

  useEffect(() => {
    if (!IS_TAURI) return;
    let cancelled = false;

    import("@tauri-apps/api/core")
      .then(({ invoke }) => invoke<HostInfo>("host_info"))
      .then((result) => {
        if (!cancelled) setInfo(result);
      })
      .catch(() => {
        /* host_info is advisory; the app works without it */
      });

    return () => {
      cancelled = true;
    };
  }, []);

  return info;
}
