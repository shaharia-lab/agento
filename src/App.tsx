import { useCallback, useEffect, useMemo, useState } from "react";
import { TitleBar } from "./components/TitleBar";
import { Sidebar } from "./components/Sidebar";
import { StatusBar } from "./components/StatusBar";
import { CommandPalette, type Command } from "./components/CommandPalette";
import { ChatsView } from "./views/ChatsView";
import { AgentsView } from "./views/AgentsView";
import { IntegrationsView } from "./views/IntegrationsView";
import { TasksView } from "./views/TasksView";
import { JobsView } from "./views/JobsView";
import { SessionsView } from "./views/SessionsView";
import { AnalyticsView } from "./views/AnalyticsView";
import { SettingsView } from "./views/SettingsView";
import { AboutView } from "./views/AboutView";
import { GatewayOverviewView } from "./views/gateway/OverviewView";
import { GatewayProvidersView } from "./views/gateway/ProvidersView";
import { GatewayModelsView } from "./views/gateway/ModelsView";
import { GatewayUsageView } from "./views/gateway/UsageView";
import { GatewaySettingsView } from "./views/gateway/SettingsView";
import { SECTIONS, VIEW_TITLES, type ViewId } from "./lib/nav";
import { useAppStats } from "./lib/stats";
import { useHostInfo } from "./lib/host";
import { useBackgroundUpdate } from "./lib/useBackgroundUpdate";
import { compactNumber, usd } from "./lib/format";
import { Icon } from "./lib/icons";
import {
  IS_MAC,
  MOD,
  onMenuAction,
  onWindowFocus,
  openExternal,
  setWindowTheme,
} from "./lib/tauri";

const REPO_URL = "https://github.com/shaharia-lab/agento";

type Theme = "light" | "dark" | "system";

export default function App() {
  const [view, setView] = useState<ViewId>("chats");
  const [history, setHistory] = useState<ViewId[]>(["chats"]);
  const [cursor, setCursor] = useState(0);

  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [inspectorOpen, setInspectorOpen] = useState(true);
  const [paletteOpen, setPaletteOpen] = useState(false);
  // Bumped by every "New Chat" entry point; ChatsView opens a draft on change.
  const [newChatNonce, setNewChatNonce] = useState(0);
  const [focused, setFocused] = useState(true);
  const [theme, setTheme] = useState<Theme>(
    () => (localStorage.getItem("agento.theme") as Theme) || "system"
  );

  /* --- Theme ------------------------------------------------------------- */
  useEffect(() => {
    const root = document.documentElement;
    if (theme === "system") root.removeAttribute("data-theme");
    else root.setAttribute("data-theme", theme);
    localStorage.setItem("agento.theme", theme);
    // The OS draws the window chrome now, so an explicit choice must reach
    // the native window too, not just the document's tokens.
    setWindowTheme(theme === "system" ? null : theme);
  }, [theme]);

  /* --- Window focus drives the selection highlight ----------------------- */
  useEffect(() => onWindowFocus(setFocused), []);

  /* --- Navigation with history ------------------------------------------- */
  const navigate = useCallback(
    (id: ViewId) => {
      setView(id);
      setHistory((h) => {
        const trimmed = h.slice(0, cursor + 1);
        if (trimmed[trimmed.length - 1] === id) return trimmed;
        setCursor(trimmed.length);
        return [...trimmed, id];
      });
    },
    [cursor]
  );

  const goBack = useCallback(() => {
    if (cursor === 0) return;
    const next = cursor - 1;
    setCursor(next);
    setView(history[next]);
  }, [cursor, history]);

  const goForward = useCallback(() => {
    if (cursor >= history.length - 1) return;
    const next = cursor + 1;
    setCursor(next);
    setView(history[next]);
  }, [cursor, history]);

  // "New Chat" everywhere (sidebar, menu, ⌘N, palette) means "go to Chats AND
  // open a fresh draft" — navigating alone left the button looking dead.
  const newChat = useCallback(() => {
    navigate("chats");
    setNewChatNonce((n) => n + 1);
  }, [navigate]);

  const stats = useAppStats();
  const host = useHostInfo();
  const update = useBackgroundUpdate(host?.can_self_update);

  // Only warn once the check has actually run — `undefined` means "not yet".
  const claudeMissing = host !== undefined && host.claude_cli === null;
  const [claudeNoticeDismissed, setClaudeNoticeDismissed] = useState(false);

  /* --- Commands ---------------------------------------------------------- */
  const commands = useMemo<Command[]>(() => {
    const nav: Command[] = SECTIONS.flatMap((s) => s.items).map((item) => ({
      id: `go:${item.id}`,
      label: `Go to ${item.label}`,
      group: "Navigate",
      icon: item.icon,
      run: () => navigate(item.id),
    }));

    return [
      {
        id: "new-chat",
        label: "New Chat",
        group: "Actions",
        icon: "plus",
        shortcut: `${MOD} N`,
        run: newChat,
      },
      {
        id: "toggle-sidebar",
        label: "Toggle Sidebar",
        group: "View",
        icon: "sidebar",
        shortcut: `${MOD} B`,
        run: () => setSidebarOpen((s) => !s),
      },
      {
        id: "toggle-inspector",
        label: "Toggle Inspector",
        group: "View",
        icon: "inspector",
        shortcut: `${MOD} I`,
        run: () => setInspectorOpen((s) => !s),
      },
      {
        id: "theme-light",
        label: "Appearance: Light",
        group: "View",
        icon: "palette",
        run: () => setTheme("light"),
      },
      {
        id: "theme-dark",
        label: "Appearance: Dark",
        group: "View",
        icon: "palette",
        run: () => setTheme("dark"),
      },
      {
        id: "theme-system",
        label: "Appearance: Match System",
        group: "View",
        icon: "palette",
        run: () => setTheme("system"),
      },
      {
        id: "settings",
        label: "Open Settings",
        group: "Actions",
        icon: "gear",
        shortcut: `${MOD} ,`,
        run: () => navigate("settings"),
      },
      ...nav,
    ];
  }, [navigate, newChat]);

  /* --- Global shortcuts -------------------------------------------------- */
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // ⌘ on macOS, Ctrl elsewhere — accepting either hijacks macOS's own
      // Ctrl+B/K/N text-editing bindings and Win+K on Windows.
      const mod = IS_MAC ? e.metaKey : e.ctrlKey;
      if (!mod || e.altKey) return;

      const k = e.key.toLowerCase();

      // While the user is typing, only the palette and Settings may steal a
      // chord; Ctrl/⌘+B or +N inside the composer must not navigate away.
      const t = e.target as HTMLElement | null;
      const typing =
        !!t &&
        (t.tagName === "INPUT" ||
          t.tagName === "TEXTAREA" ||
          t.tagName === "SELECT" ||
          t.isContentEditable);
      if (typing && k !== "k" && k !== ",") return;

      if (k === "k") {
        e.preventDefault();
        setPaletteOpen((p) => !p);
      } else if (k === "b") {
        e.preventDefault();
        setSidebarOpen((s) => !s);
      } else if (k === "i") {
        e.preventDefault();
        setInspectorOpen((s) => !s);
      } else if (k === ",") {
        e.preventDefault();
        navigate("settings");
      } else if (k === "n") {
        e.preventDefault();
        newChat();
      } else if (k === "[") {
        e.preventDefault();
        goBack();
      } else if (k === "]") {
        e.preventDefault();
        goForward();
      } else if (k >= "1" && k <= "7") {
        e.preventDefault();
        const flat = SECTIONS.flatMap((s) => s.items);
        const target = flat[Number(k) - 1];
        if (target) navigate(target.id);
      }
    };

    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [goBack, goForward, navigate, newChat]);

  /* --- Native menu ------------------------------------------------------- */
  useEffect(
    () =>
      onMenuAction((id) => {
        if (id.startsWith("go:")) {
          navigate(id.slice(3) as ViewId);
          return;
        }
        switch (id) {
          case "new_chat":
            newChat();
            break;
          case "new_agent":
            navigate("agents");
            break;
          case "new_task":
            navigate("tasks");
            break;
          case "settings":
            navigate("settings");
            break;
          case "toggle_sidebar":
            setSidebarOpen((s) => !s);
            break;
          case "toggle_inspector":
            setInspectorOpen((s) => !s);
            break;
          case "palette":
            setPaletteOpen(true);
            break;
          case "theme_light":
            setTheme("light");
            break;
          case "theme_dark":
            setTheme("dark");
            break;
          case "theme_system":
            setTheme("system");
            break;
          case "go_back":
            goBack();
            break;
          case "go_forward":
            goForward();
            break;
          case "docs":
            openExternal(`${REPO_URL}#readme`);
            break;
          case "github":
            openExternal(REPO_URL);
            break;
        }
      }),
    [goBack, goForward, navigate, newChat]
  );

  const counts: Partial<Record<ViewId, number>> = {
    chats: stats.chats,
    agents: stats.agents,
    tasks: stats.activeTasks,
  };

  return (
    <div className={`window ${focused ? "window--focused" : ""}`}>
      <TitleBar
        title={VIEW_TITLES[view]}
        subtitle={subtitleFor(view, stats)}
        sidebarOpen={sidebarOpen}
        onToggleSidebar={() => setSidebarOpen((s) => !s)}
        onOpenPalette={() => setPaletteOpen(true)}
        onBack={goBack}
        onForward={goForward}
        canBack={cursor > 0}
        canForward={cursor < history.length - 1}
      />

      <div className="body">
        <Sidebar
          open={sidebarOpen}
          active={view}
          onSelect={navigate}
          counts={counts}
          onNewChat={newChat}
        />

        <main className="main">
          {update.available && (
            <div className="banner">
              <Icon name="sparkle" size={14} />
              <span>
                {update.installing
                  ? `Installing version ${update.available.version} — Agento will restart.`
                  : `Version ${update.available.version} is available.`}
              </span>
              {!update.installing && (
                <button
                  className="btn"
                  style={{ marginLeft: "auto", height: 20 }}
                  onClick={() => navigate("about")}
                >
                  Details
                </button>
              )}
              <button
                className="iconbtn"
                style={update.installing ? { marginLeft: "auto" } : undefined}
                onClick={update.dismiss}
                title="Dismiss"
              >
                <Icon name="close" size={13} />
              </button>
            </div>
          )}
          {claudeMissing && !claudeNoticeDismissed && (
            <div className="banner">
              <Icon name="alert" size={14} />
              <span>
                Claude Code is not installed, so agents cannot run. Install it
                with <code>npm i -g @anthropic-ai/claude-code</code>, sign in
                with <code>claude</code>, then restart Agento.
              </span>
              <button
                className="iconbtn"
                style={{ marginLeft: "auto" }}
                onClick={() => setClaudeNoticeDismissed(true)}
                title="Dismiss"
              >
                <Icon name="close" size={13} />
              </button>
            </div>
          )}
          {view === "chats" && (
            <ChatsView inspectorOpen={inspectorOpen} newChatNonce={newChatNonce} />
          )}
          {view === "agents" && <AgentsView inspectorOpen={inspectorOpen} />}
          {view === "integrations" && (
            <IntegrationsView inspectorOpen={inspectorOpen} />
          )}
          {view === "tasks" && <TasksView inspectorOpen={inspectorOpen} />}
          {view === "jobs" && <JobsView inspectorOpen={inspectorOpen} />}
          {view === "sessions" && <SessionsView inspectorOpen={inspectorOpen} />}
          {(view === "tokens" || view === "usage" || view === "insights") && (
            <AnalyticsView mode={view} inspectorOpen={inspectorOpen} />
          )}
          {/* The Overview takes `navigate` because its bind-failure card has
              exactly one useful action — change the port — and that lives in a
              sibling view rather than a pane of its own. */}
          {view === "gateway" && (
            <GatewayOverviewView
              inspectorOpen={inspectorOpen}
              onNavigate={navigate}
            />
          )}
          {view === "gateway-providers" && (
            <GatewayProvidersView inspectorOpen={inspectorOpen} />
          )}
          {view === "gateway-models" && (
            <GatewayModelsView inspectorOpen={inspectorOpen} />
          )}
          {view === "gateway-usage" && (
            <GatewayUsageView inspectorOpen={inspectorOpen} />
          )}
          {view === "gateway-settings" && (
            <GatewaySettingsView inspectorOpen={inspectorOpen} />
          )}
          {view === "settings" && (
            <SettingsView theme={theme} onThemeChange={setTheme} />
          )}
          {view === "about" && <AboutView />}
        </main>
      </div>

      <StatusBar
        running={stats.runningJobs}
        connected={stats.connected}
        model={stats.model}
        tokensToday={compactNumber(stats.tokensToday)}
        costToday={usd(stats.costToday)}
        inspectorOpen={inspectorOpen}
        onToggleInspector={() => setInspectorOpen((s) => !s)}
        theme={theme}
        onCycleTheme={() =>
          setTheme((t) =>
            t === "system" ? "light" : t === "light" ? "dark" : "system"
          )
        }
      />

      {paletteOpen && (
        <CommandPalette
          commands={commands}
          onClose={() => setPaletteOpen(false)}
        />
      )}
    </div>
  );
}

function subtitleFor(
  view: ViewId,
  stats: { chats: number; agents: number; activeTasks: number }
): string | undefined {
  switch (view) {
    case "chats":
      return plural(stats.chats, "conversation");
    case "agents":
      return plural(stats.agents, "agent");
    case "tasks":
      return `${stats.activeTasks} active`;
    default:
      return undefined;
  }
}

function plural(n: number, noun: string): string {
  return `${n} ${noun}${n === 1 ? "" : "s"}`;
}
