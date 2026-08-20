/* ============================================================================
   Cross-view counters for the sidebar and status bar.

   Kept in one place so every surface reads the same numbers, and polled on a
   slow interval — these are ambient indicators, not the reason anyone opened
   the app.
   ========================================================================== */

import { useCallback, useEffect, useState } from "react";
import { api, qs } from "./api";
import type {
  AnalyticsReport,
  ChatSession,
  Agent,
  JobHistory,
  ScheduledTask,
  SettingsResponse,
} from "./types";

export interface AppStats {
  chats: number;
  agents: number;
  activeTasks: number;
  runningJobs: number;
  model: string;
  tokensToday: number;
  costToday: number;
  connected: boolean;
  error?: string;
}

const EMPTY: AppStats = {
  chats: 0,
  agents: 0,
  activeTasks: 0,
  runningJobs: 0,
  model: "—",
  tokensToday: 0,
  costToday: 0,
  connected: false,
};

export function useAppStats(intervalMs = 30_000): AppStats & { reload(): void } {
  const [stats, setStats] = useState<AppStats>(EMPTY);

  const load = useCallback(async (signal?: AbortSignal) => {
    try {
      const startOfDay = new Date();
      startOfDay.setHours(0, 0, 0, 0);
      const tz = Intl.DateTimeFormat().resolvedOptions().timeZone;

      // Settled rather than all: one failing endpoint must not blank the whole
      // status bar, and analytics is by far the slowest of these.
      const [chats, agents, tasks, jobs, settings, analytics] =
        await Promise.allSettled([
          api.get<ChatSession[]>("/chats", signal),
          api.get<Agent[]>("/agents", signal),
          api.get<ScheduledTask[]>("/tasks", signal),
          api.get<JobHistory[]>("/job-history" + qs({ limit: 50 }), signal),
          api.get<SettingsResponse>("/settings", signal),
          api.get<AnalyticsReport>(
            "/claude-analytics" +
              qs({ from: startOfDay.toISOString(), tz }),
            signal
          ),
        ]);

      setStats({
        chats: len(chats),
        agents: len(agents),
        activeTasks:
          tasks.status === "fulfilled"
            ? (tasks.value ?? []).filter((t) => t.status === "active").length
            : 0,
        runningJobs:
          jobs.status === "fulfilled"
            ? (jobs.value ?? []).filter((j) => j.status === "running").length
            : 0,
        model:
          settings.status === "fulfilled"
            ? settings.value?.settings?.default_model || "—"
            : "—",
        tokensToday:
          analytics.status === "fulfilled"
            ? analytics.value?.summary?.total_tokens ?? 0
            : 0,
        costToday:
          analytics.status === "fulfilled"
            ? analytics.value?.summary?.estimated_cost_usd ?? 0
            : 0,
        // Any endpoint answering at all proves the backend is up; a single
        // handler erroring is a different problem from a dead server.
        connected: [chats, agents, tasks, settings].some(
          (r) => r.status === "fulfilled"
        ),
      });
    } catch {
      setStats((s) => ({ ...s, connected: false }));
    }
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    load(controller.signal);

    const id = setInterval(() => {
      if (document.visibilityState === "visible") load();
    }, intervalMs);

    // Coming back to the window refreshes immediately. Without this the
    // counters can sit a whole interval behind after work done elsewhere —
    // and while the window is hidden the interval skips entirely, so "a whole
    // interval" is really "until you next look at it".
    const onVisible = () => {
      if (document.visibilityState === "visible") load();
    };
    document.addEventListener("visibilitychange", onVisible);
    window.addEventListener("focus", onVisible);

    return () => {
      controller.abort();
      clearInterval(id);
      document.removeEventListener("visibilitychange", onVisible);
      window.removeEventListener("focus", onVisible);
    };
  }, [load, intervalMs]);

  return { ...stats, reload: () => load() };
}

function len(result: PromiseSettledResult<unknown[] | null>): number {
  return result.status === "fulfilled" ? (result.value ?? []).length : 0;
}
