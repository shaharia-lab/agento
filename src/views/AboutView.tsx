import { useCallback, useEffect, useState } from "react";
import { api } from "../lib/api";
import { useResource } from "../lib/hooks";
import { useHostInfo } from "../lib/host";
import type { VersionInfo } from "../lib/types";
import { Icon } from "../lib/icons";
import { dateTime } from "../lib/format";
import { IS_TAURI, openExternal } from "../lib/tauri";
import {
  RELEASES_URL,
  checkForUpdate,
  installUpdate,
  type UpdateState,
} from "../lib/updater";
import {
  UPDATE_PREF_KEY,
  loadUpdatePref,
  type UpdatePref,
} from "../lib/updatePref";

export function AboutView() {
  const version = useResource<VersionInfo>(
    (signal) => api.get<VersionInfo>("/version", signal),
    []
  );
  const host = useHostInfo();

  const [state, setState] = useState<UpdateState>({ kind: "idle" });
  const [pref, setPref] = useState<UpdatePref>(loadUpdatePref);

  // Keep in step with the Settings pane, which writes the same key.
  useEffect(() => {
    const onStorage = (e: StorageEvent) => {
      if (e.key === UPDATE_PREF_KEY) setPref(loadUpdatePref());
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, []);

  const check = useCallback(async () => {
    setState({ kind: "checking" });
    try {
      const update = await checkForUpdate();
      setState(update ? { kind: "available", update } : { kind: "current" });
    } catch (err) {
      setState({
        kind: "error",
        message: err instanceof Error ? err.message : String(err),
      });
    }
  }, []);

  // Check once on open unless the user has turned checks off entirely.
  useEffect(() => {
    if (!IS_TAURI || pref === "never") return;
    check();
  }, [check, pref]);

  const install = async () => {
    setState({ kind: "downloading", percent: 0 });
    try {
      await installUpdate((percent) => setState({ kind: "downloading", percent }));
      setState({ kind: "installed" });
    } catch (err) {
      setState({
        kind: "error",
        message: err instanceof Error ? err.message : String(err),
      });
    }
  };

  const open = (url: string) => void openExternal(url);
  const busy = state.kind === "checking" || state.kind === "downloading";

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

          <UpdateStatus
            state={state}
            canSelfUpdate={host?.can_self_update ?? false}
            installKind={host?.install_kind}
            onInstall={install}
            onOpenReleases={() => open(RELEASES_URL)}
          />

          <div className="row" style={{ gap: "var(--sp-4)", marginTop: "var(--sp-4)" }}>
            <button
              className="btn btn--lg"
              onClick={() => open("https://github.com/shaharia-lab/agento")}
            >
              <Icon name="star" size={14} />
              Star on GitHub
            </button>
            <button className="btn btn--lg" onClick={() => open(RELEASES_URL)}>
              <Icon name="external" size={14} />
              Release notes
            </button>
            {IS_TAURI && (
              <button
                className="btn btn--lg btn--primary"
                onClick={check}
                disabled={busy}
              >
                <Icon name="refresh" size={14} />
                {state.kind === "checking" ? "Checking…" : "Check for updates"}
              </button>
            )}
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

function UpdateStatus({
  state,
  canSelfUpdate,
  installKind,
  onInstall,
  onOpenReleases,
}: {
  state: UpdateState;
  canSelfUpdate: boolean;
  installKind: string | undefined;
  onInstall(): void;
  onOpenReleases(): void;
}) {
  if (state.kind === "idle") return null;

  if (state.kind === "checking") {
    return <span className="badge">Checking for updates…</span>;
  }

  if (state.kind === "current") {
    return <span className="badge badge--green">Up to date</span>;
  }

  if (state.kind === "error") {
    return (
      <div className="col" style={{ gap: "var(--sp-3)", alignItems: "center" }}>
        <span className="badge badge--red">Update check failed</span>
        <span style={{ fontSize: "var(--text-sm)", color: "var(--fg-tertiary)" }}>
          {state.message}
        </span>
      </div>
    );
  }

  if (state.kind === "downloading") {
    return (
      <div className="col" style={{ gap: "var(--sp-3)", alignItems: "center", width: 260 }}>
        <span className="badge badge--accent">
          {state.percent === null
            ? "Downloading…"
            : `Downloading ${Math.round(state.percent)}%`}
        </span>
        <div className="meter" style={{ width: "100%", display: "block" }}>
          <div
            className="meter__fill"
            style={{
              width: state.percent === null ? "100%" : `${state.percent}%`,
              opacity: state.percent === null ? 0.5 : 1,
            }}
          />
        </div>
      </div>
    );
  }

  if (state.kind === "installed") {
    return <span className="badge badge--green">Installed — restarting…</span>;
  }

  // available
  return (
    <div className="col" style={{ gap: "var(--sp-4)", alignItems: "center" }}>
      <span className="badge badge--amber">
        Version {state.update.version} is available
      </span>
      {canSelfUpdate ? (
        <button className="btn btn--primary btn--lg" onClick={onInstall}>
          <Icon name="arrowDown" size={14} />
          Install and restart
        </button>
      ) : (
        <div className="col" style={{ gap: "var(--sp-3)", alignItems: "center" }}>
          <span
            style={{
              fontSize: "var(--text-sm)",
              color: "var(--fg-tertiary)",
              maxWidth: 380,
              lineHeight: "var(--leading-normal)",
            }}
          >
            {installKind === "package"
              ? "This copy was installed from a system package, so your package manager owns the update. Run your usual upgrade command, or download the new release directly."
              : "Download the new release to update."}
          </span>
          <button className="btn btn--lg" onClick={onOpenReleases}>
            <Icon name="external" size={14} />
            Open releases
          </button>
        </div>
      )}
    </div>
  );
}
