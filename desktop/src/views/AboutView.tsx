import { useState } from "react";
import { api } from "../lib/api";
import { useResource } from "../lib/hooks";
import type { VersionInfo } from "../lib/types";
import { Icon } from "../lib/icons";
import { dateTime } from "../lib/format";

interface UpdateCheck {
  update_available?: boolean;
  latest_version?: string;
  release_url?: string;
  current_version?: string;
}

export function AboutView() {
  const version = useResource<VersionInfo>(
    (signal) => api.get<VersionInfo>("/version", signal),
    []
  );

  const [check, setCheck] = useState<UpdateCheck>();
  const [checking, setChecking] = useState(false);
  const [checkError, setCheckError] = useState<string>();

  const checkForUpdates = async () => {
    setChecking(true);
    setCheckError(undefined);
    try {
      setCheck(await api.get<UpdateCheck>("/version/update-check"));
    } catch (err) {
      setCheckError(err instanceof Error ? err.message : String(err));
    } finally {
      setChecking(false);
    }
  };

  const open = (url: string) => window.open(url, "_blank", "noopener");

  return (
    <div className="panes">
      <div className="pane-detail">
        <div
          className="col scroll"
          style={{
            flex: 1,
            alignItems: "center",
            justifyContent: "center",
            gap: "var(--sp-5)",
            padding: "var(--sp-10)",
            textAlign: "center",
          }}
        >
          <div
            style={{
              width: 72,
              height: 72,
              borderRadius: 18,
              background: "linear-gradient(160deg, var(--accent), var(--purple))",
              display: "grid",
              placeItems: "center",
              color: "#fff",
              fontSize: 34,
              fontWeight: 600,
              boxShadow: "var(--shadow-md)",
            }}
          >
            A
          </div>

          <div>
            <div style={{ fontSize: "var(--text-xl)", fontWeight: 600 }}>Agento</div>
            <div style={{ color: "var(--fg-tertiary)", marginTop: 2 }}>
              Personal AI agent platform for Claude Code
            </div>
          </div>

          <div className="row" style={{ gap: "var(--sp-4)" }}>
            <span className="badge">
              {version.loading
                ? "Loading…"
                : `Version ${version.data?.version ?? "unknown"}`}
            </span>
            <span className="badge badge--accent">Desktop</span>
          </div>

          {version.data?.commit && (
            <div
              className="mono selectable"
              style={{ fontSize: "var(--text-sm)", color: "var(--fg-tertiary)" }}
            >
              {version.data.commit.slice(0, 12)}
              {version.data.build_date && ` · ${dateTime(version.data.build_date)}`}
            </div>
          )}

          <p
            className="selectable"
            style={{
              maxWidth: 440,
              color: "var(--fg-secondary)",
              lineHeight: "var(--leading-relaxed)",
              marginTop: "var(--sp-3)",
            }}
          >
            Agento runs your agents locally — scheduling them, giving them tools,
            and keeping every conversation and job on your own machine.
          </p>

          {check && (
            <div
              className={`badge ${
                check.update_available ? "badge--amber" : "badge--green"
              }`}
            >
              {check.update_available
                ? `Update available: ${check.latest_version}`
                : "Up to date"}
            </div>
          )}
          {checkError && (
            <div className="badge badge--red">{checkError}</div>
          )}

          <div className="row" style={{ gap: "var(--sp-4)", marginTop: "var(--sp-4)" }}>
            <button
              className="btn btn--lg"
              onClick={() => open("https://github.com/shaharia-lab/agento")}
            >
              <Icon name="star" size={14} />
              Star on GitHub
            </button>
            <button
              className="btn btn--lg"
              onClick={() =>
                open(
                  check?.release_url ??
                    "https://github.com/shaharia-lab/agento/releases"
                )
              }
            >
              <Icon name="external" size={14} />
              Release notes
            </button>
            <button
              className="btn btn--lg btn--primary"
              onClick={checkForUpdates}
              disabled={checking}
            >
              <Icon name="refresh" size={14} />
              {checking ? "Checking…" : "Check for updates"}
            </button>
          </div>

          <div
            style={{
              marginTop: "var(--sp-8)",
              fontSize: "var(--text-sm)",
              color: "var(--fg-quaternary)",
            }}
          >
            Tauri 2 · Rust · React — © 2026 Shaharia Lab
          </div>
        </div>
      </div>
    </div>
  );
}
