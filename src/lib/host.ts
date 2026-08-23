/* ============================================================================
   Host facts resolved by the Rust side at startup.
   ========================================================================== */

import { useEffect, useState } from "react";
import { hostInfo, type HostInfo } from "./tauri";

export type { HostInfo };

/**
 * Everything the installer ships is self-contained except the Claude Code CLI:
 * the backend runs agents by spawning it. Outside Tauri we cannot look at the
 * filesystem, so assume it is present rather than showing a false warning in a
 * browser tab.
 *
 * The fetch itself lives in `tauri.ts` and is memoized there, because `api.ts`
 * needs the same answer for the `/api` bearer token (#400) and cannot import a
 * React hook. This is the view-facing wrapper over that one call.
 */
export function useHostInfo(): HostInfo | undefined {
  const [info, setInfo] = useState<HostInfo>();

  useEffect(() => {
    let cancelled = false;

    hostInfo().then((result) => {
      if (!cancelled && result) setInfo(result);
    });

    return () => {
      cancelled = true;
    };
  }, []);

  return info;
}
