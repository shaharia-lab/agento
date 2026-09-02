import { createContext, useContext } from "react";
import type { IconName } from "./icons";

export type ViewId =
  | "chats"
  | "agents"
  | "integrations"
  | "tasks"
  | "jobs"
  | "sessions"
  | "tokens"
  | "usage"
  | "insights"
  | "gateway"
  | "gateway-providers"
  | "gateway-models"
  | "gateway-usage"
  | "gateway-settings"
  | "settings"
  | "about";

export interface NavItem {
  id: ViewId;
  label: string;
  icon: IconName;
  badge?: string;
}

export interface NavSection {
  caption: string | null;
  items: NavItem[];
}

export const SECTIONS: NavSection[] = [
  {
    caption: null,
    items: [
      { id: "chats", label: "Chats", icon: "chat" },
      { id: "agents", label: "Agents", icon: "agent" },
      { id: "integrations", label: "Integrations", icon: "plug" },
    ],
  },
  {
    caption: "Automation",
    items: [
      { id: "tasks", label: "Scheduled Tasks", icon: "task" },
      { id: "jobs", label: "Job History", icon: "history" },
    ],
  },
  {
    caption: "Claude Usage",
    items: [{ id: "sessions", label: "Sessions", icon: "clock" }],
  },
  {
    caption: "Analytics",
    items: [
      { id: "tokens", label: "Token Usage", icon: "chart" },
      { id: "usage", label: "General Usage", icon: "grid" },
      { id: "insights", label: "Insights", icon: "bulb", badge: "Beta" },
    ],
  },
  // Its own section, sharing nothing with Claude Usage or Analytics — the
  // gateway spends the user's *provider* credits, where everything above it
  // reports on Claude Code runs. Mixing the two was ruled out at design time.
  {
    caption: "LLM Gateway",
    items: [
      { id: "gateway", label: "Overview", icon: "zap" },
      { id: "gateway-providers", label: "Providers", icon: "database" },
      { id: "gateway-models", label: "Models", icon: "layers" },
      { id: "gateway-usage", label: "Usage", icon: "chart" },
      { id: "gateway-settings", label: "Gateway Settings", icon: "gear" },
    ],
  },
];

export const VIEW_TITLES: Record<ViewId, string> = {
  chats: "Chats",
  agents: "Agents",
  integrations: "Integrations",
  tasks: "Scheduled Tasks",
  jobs: "Job History",
  sessions: "Claude Sessions",
  tokens: "Token Usage",
  usage: "General Usage",
  insights: "Insights",
  gateway: "LLM Gateway",
  "gateway-providers": "Gateway Providers",
  "gateway-models": "Gateway Models",
  "gateway-usage": "Gateway Usage",
  "gateway-settings": "Gateway Settings",
  settings: "Settings",
  about: "About Agento",
};

/* --- Cross-view navigation ------------------------------------------------ */

/**
 * What a view wants the *next* view to open, beyond the section itself.
 *
 * Deliberately one optional row id per destination: this is a hand-off, not a
 * router, and it must not grow query state, filters or scroll positions. A
 * second destination adds a second optional field here, and nothing else.
 */
export interface NavTarget {
  /** A chat `chats` should preselect on arrival (#485). */
  chatId?: string;
  /** A session `sessions` should open the transcript of on arrival (#536). */
  sessionId?: string;
}

export type NavigateFn = (id: ViewId, target?: NavTarget) => void;

/**
 * `App`'s `navigate`, reachable from a nested view without threading a callback
 * through every parent.
 *
 * The prop form is still the norm — three gateway views take `onNavigate`
 * because they are rendered directly by `App` and have nothing to thread
 * through. This exists for the other case: `SessionsView`'s inspector and
 * `SessionDetail` are several levels down, and "Continue in chat" needs to land
 * the user in the Chats view.
 */
const NavContext = createContext<NavigateFn>(() => {
  // Not fatal — but a hand-off that silently does nothing is exactly the bug
  // #485 was filed for, so it must not be invisible.
  console.warn("navigate() called outside a NavProvider; nothing happened");
});

export const NavProvider = NavContext.Provider;

export function useNavigate(): NavigateFn {
  return useContext(NavContext);
}
