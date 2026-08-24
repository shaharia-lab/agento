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
  "gateway-settings": "Gateway Settings",
  settings: "Settings",
  about: "About Agento",
};
