import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";
import { api, ApiError, qs } from "../lib/api";
import { describeError, useResource, type Resource } from "../lib/hooks";
import { dateTime, relativeTime, tildePath, usd } from "../lib/format";
import { Icon, type IconName } from "../lib/icons";
import { Checkbox, Empty, FormRow, Switch } from "../components/ui";
import { useHostInfo } from "../lib/host";
import {
  UPDATE_PREF_OPTIONS,
  loadUpdatePref,
  saveUpdatePref,
  type UpdatePref,
} from "../lib/updatePref";
import type {
  ClaudeSettingsProfile,
  FSEntry,
  NotificationLogEntry,
  PricingCatalog,
  UserSettings,
  VersionInfo,
} from "../lib/types";
import "../styles/settings.css";

/* ============================================================================
   Wire shapes this view owns.

   GET /settings, GET /monitoring and GET /version/update-check all answer with
   an envelope rather than the bare record, so the record is unwrapped here.
   ========================================================================== */

/**
 * `locked` maps a field name to the environment variable that pinned it — the
 * variable name is what the pane has to show, so the values are read as the
 * strings the server actually sends rather than as flags.
 */
interface SettingsEnvelope {
  settings: UserSettings;
  locked: Record<string, string>;
  model_from_env: boolean;
}

interface MonitoringConfig {
  enabled: boolean;
  metrics_exporter: string;
  logs_exporter: string;
  otlp_endpoint: string;
  otlp_headers?: Record<string, string>;
  otlp_insecure: boolean;
  metric_export_interval_ms: number;
}

interface MonitoringEnvelope {
  settings: MonitoringConfig;
  locked: Record<string, string>;
  env_locked: boolean;
}

interface SMTPConfig {
  host: string;
  port: number;
  username: string;
  password: string;
  from_address: string;
  to_addresses: string;
  encryption: string;
}

interface NotificationSettings {
  enabled: boolean;
  provider: SMTPConfig;
  preferences: {
    scheduled_tasks: { on_finished?: boolean; on_failed?: boolean };
  };
}

interface ClaudeConfigDirs {
  indexed: string[] | null;
  candidates: string[] | null;
  default: string;
}

interface UpdateCheck {
  current_version: string;
  latest_version: string;
  release_url: string;
  update_available: boolean;
}

interface FSListing {
  path: string;
  parent: string;
  entries: FSEntry[] | null;
}

/** The sentinel the server accepts to mean "keep the stored password". */
const PASSWORD_UNCHANGED = "***";

const MODELS = [
  { value: "sonnet", label: "Sonnet" },
  { value: "opus", label: "Opus" },
  { value: "haiku", label: "Haiku" },
];

const IDLE_GAP_MIN = 1;
const IDLE_GAP_MAX = 240;

type Pane =
  | "general"
  | "claude"
  | "appearance"
  | "notifications"
  | "data"
  | "pricing"
  | "advanced";

const PANES: { id: Pane; label: string; icon: IconName }[] = [
  { id: "general", label: "General", icon: "gear" },
  { id: "claude", label: "Claude", icon: "sparkle" },
  { id: "appearance", label: "Appearance", icon: "palette" },
  { id: "notifications", label: "Notifications", icon: "bell" },
  { id: "data", label: "Data", icon: "database" },
  { id: "pricing", label: "Pricing", icon: "dollar" },
  { id: "advanced", label: "Advanced", icon: "cpu" },
];

/* ============================================================================
   An editable server record: the fetched copy, plus a draft the pane mutates.
   ========================================================================== */

interface Editable<T> {
  loading: boolean;
  error: string | undefined;
  server: T | undefined;
  setServer: Dispatch<SetStateAction<T | undefined>>;
  draft: T | undefined;
  setDraft: Dispatch<SetStateAction<T | undefined>>;
  dirty: boolean;
  revert(): void;
  reload(): void;
}

function useEditable<T>(
  fetcher: (signal: AbortSignal) => Promise<T>,
  deps: unknown[]
): Editable<T> {
  const res = useResource(fetcher, deps);
  const [server, setServer] = useState<T>();
  const [draft, setDraft] = useState<T>();

  useEffect(() => {
    if (res.data !== undefined) setServer(res.data);
  }, [res.data]);

  // The serialised server copy is both the reset source and the dirty
  // comparison, so a re-fetch that changes nothing does not discard an edit.
  const key = server === undefined ? "" : JSON.stringify(server);

  useEffect(() => {
    setDraft(key === "" ? undefined : (JSON.parse(key) as T));
  }, [key]);

  const revert = useCallback(() => {
    setDraft(key === "" ? undefined : (JSON.parse(key) as T));
  }, [key]);

  return {
    loading: res.loading,
    error: res.error,
    server,
    setServer,
    draft,
    setDraft,
    dirty: draft !== undefined && JSON.stringify(draft) !== key,
    revert,
    reload: res.reload,
  };
}

/* ============================================================================
   Settings
   ========================================================================== */

/**
 * Preferences use a toolbar of icon+label tabs across the top — the native
 * settings idiom — rather than a second sidebar nested inside the content.
 */
export function SettingsView({
  theme,
  onThemeChange,
}: {
  theme: "light" | "dark" | "system";
  onThemeChange(t: "light" | "dark" | "system"): void;
}) {
  const [pane, setPane] = useState<Pane>("general");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string>();
  const [notice, setNotice] = useState<string>();

  const settings = useEditable<SettingsEnvelope>(
    (signal) => api.get<SettingsEnvelope>("/settings", signal),
    []
  );
  // Read-only since #309: this build exports no telemetry, so there is nothing
  // for a save here to take effect on. The stored config is still worth showing
  // — an `agento web` on the same data dir uses it, and `locked` says which
  // OTEL_* variables have pinned a field.
  const monitoring = useResource<MonitoringEnvelope>(
    (signal) => api.get<MonitoringEnvelope>("/monitoring", signal),
    []
  );
  const notifications = useEditable<NotificationSettings>(
    (signal) => api.get<NotificationSettings>("/notifications/settings", signal),
    []
  );

  // Held apart from the draft so the stored password is never re-rendered:
  // null means "not touched", and the sentinel goes back to the server.
  const [passwordEdit, setPasswordEdit] = useState<string | null>(null);

  const user = settings.draft?.settings;
  const locked = settings.server?.locked ?? {};

  const idleGapError =
    user &&
    user.idle_gap_threshold_minutes !== 0 &&
    (user.idle_gap_threshold_minutes < IDLE_GAP_MIN ||
      user.idle_gap_threshold_minutes > IDLE_GAP_MAX)
      ? `Must be 0 (use the default) or between ${IDLE_GAP_MIN} and ${IDLE_GAP_MAX}.`
      : undefined;

  const dirty = settings.dirty || notifications.dirty || passwordEdit !== null;

  const patchUser = useCallback(
    (patch: Partial<UserSettings>) =>
      settings.setDraft((prev) =>
        prev ? { ...prev, settings: { ...prev.settings, ...patch } } : prev
      ),
    [settings]
  );

  async function save() {
    if (idleGapError) return;
    setSaving(true);
    setError(undefined);
    setNotice(undefined);
    try {
      if (settings.dirty && settings.draft) {
        // PUT takes the bare record and answers with the envelope.
        const next = await api.put<SettingsEnvelope>(
          "/settings",
          settings.draft.settings
        );
        settings.setServer(next);
      }
      if ((notifications.dirty || passwordEdit !== null) && notifications.draft) {
        const body: NotificationSettings = {
          ...notifications.draft,
          provider: {
            ...notifications.draft.provider,
            password: passwordEdit ?? PASSWORD_UNCHANGED,
          },
        };
        const next = await api.put<NotificationSettings>(
          "/notifications/settings",
          body
        );
        notifications.setServer(next);
        setPasswordEdit(null);
      }
      setNotice("Settings saved.");
    } catch (err) {
      setError(describeError(err));
    } finally {
      setSaving(false);
    }
  }

  function revertAll() {
    settings.revert();
    notifications.revert();
    setPasswordEdit(null);
    setError(undefined);
    setNotice(undefined);
  }

  const loading = settings.loading && !settings.server;

  return (
    <div className="panes">
      <div className="pane-detail">
        <div className="toolbar settabs">
          {PANES.map((p) => (
            <button
              key={p.id}
              onClick={() => setPane(p.id)}
              className={`settab ${pane === p.id ? "settab--active" : ""}`}
            >
              <Icon name={p.icon} size={17} />
              <span>{p.label}</span>
            </button>
          ))}
        </div>

        <div className="scroll" style={{ flex: 1, padding: "var(--sp-9)" }}>
          {loading ? (
            <Empty icon="gear" title="Loading" text="Reading settings from the server." />
          ) : settings.error && !settings.server ? (
            <Empty
              icon="alert"
              title="Settings unavailable"
              text={settings.error}
              action={
                <button className="btn btn--lg" onClick={settings.reload}>
                  <Icon name="refresh" size={14} />
                  Try again
                </button>
              }
            />
          ) : (
            <div className="form" style={{ margin: "0 auto" }}>
              {pane === "general" && user && (
                <GeneralPane
                  user={user}
                  locked={locked}
                  modelFromEnv={settings.server?.model_from_env ?? false}
                  onPatch={patchUser}
                />
              )}

              {pane === "claude" && user && (
                <ClaudePane user={user} locked={locked} onPatch={patchUser} />
              )}

              {pane === "appearance" && user && (
                <AppearancePane
                  user={user}
                  theme={theme}
                  onThemeChange={onThemeChange}
                  onPatch={patchUser}
                />
              )}

              {pane === "notifications" && (
                <NotificationsPane
                  editable={notifications}
                  passwordEdit={passwordEdit}
                  onPasswordEdit={setPasswordEdit}
                />
              )}

              {pane === "data" && user && (
                <DataPane
                  user={user}
                  onPatch={patchUser}
                  idleGapError={idleGapError}
                />
              )}

              {pane === "pricing" && <PricingPane />}

              {pane === "advanced" && user && (
                <AdvancedPane user={user} onPatch={patchUser} monitoring={monitoring} />
              )}

              {error && (
                <div className="msgline msgline--error">
                  <span className="msgline__icon">
                    <Icon name="alert" size={13} />
                  </span>
                  <span>{error}</span>
                </div>
              )}
              {notice && !dirty && (
                <div className="msgline msgline--ok">
                  <span className="msgline__icon">
                    <Icon name="check" size={13} />
                  </span>
                  <span>{notice}</span>
                </div>
              )}

              {dirty && (
                <div className="savebar">
                  <span className="savebar__text">
                    {idleGapError ? idleGapError : "You have unsaved changes."}
                  </span>
                  <button className="btn" onClick={revertAll} disabled={saving}>
                    Revert
                  </button>
                  <button
                    className="btn btn--primary"
                    onClick={save}
                    disabled={saving || idleGapError !== undefined}
                  >
                    {saving ? "Saving…" : "Save"}
                  </button>
                </div>
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

/* --- General ------------------------------------------------------------- */

function GeneralPane({
  user,
  locked,
  modelFromEnv,
  onPatch,
}: {
  user: UserSettings;
  locked: Record<string, string>;
  modelFromEnv: boolean;
  onPatch(patch: Partial<UserSettings>): void;
}) {
  const [browsing, setBrowsing] = useState(false);

  const dirLock = locked.default_working_dir;
  const modelLock = locked.default_model;
  const urlLock = locked.public_url;

  return (
    <>
      <div className="formsec">
        <div className="formsec__title">Workspace</div>
        <FormRow
          label="Working directory"
          help={
            dirLock
              ? lockHelp(dirLock)
              : "Where agents create and edit files unless told otherwise."
          }
        >
          <div className="row" style={{ gap: "var(--sp-3)" }}>
            <label
              className={`field ${dirLock ? "field--locked" : ""}`}
              style={{ flex: 1 }}
            >
              <input
                value={user.default_working_dir}
                onChange={(e) => onPatch({ default_working_dir: e.target.value })}
                className="mono"
                disabled={!!dirLock}
                spellCheck={false}
              />
            </label>
            <button
              className="btn btn--lg"
              onClick={() => setBrowsing(true)}
              disabled={!!dirLock}
            >
              <Icon name="folder" size={14} />
              Browse…
            </button>
          </div>
        </FormRow>

        <FormRow
          label="Default model"
          help={
            modelLock
              ? lockHelp(modelLock)
              : modelFromEnv
                ? "The current value comes from the environment. Choosing another here overrides it."
                : undefined
          }
        >
          <select
            className="nselect"
            style={{ maxWidth: 200 }}
            value={user.default_model}
            onChange={(e) => onPatch({ default_model: e.target.value })}
            disabled={!!modelLock}
          >
            {MODELS.some((m) => m.value === user.default_model) ? null : (
              <option value={user.default_model}>{user.default_model}</option>
            )}
            {MODELS.map((m) => (
              <option key={m.value} value={m.value}>
                {m.label}
              </option>
            ))}
          </select>
        </FormRow>

        <FormRow
          label="Public URL"
          help={
            urlLock
              ? lockHelp(urlLock)
              : "Externally reachable URL of this instance. Required for inbound webhooks."
          }
        >
          <label className={`field ${urlLock ? "field--locked" : ""}`}>
            <span className="field__icon">
              <Icon name="globe" size={14} />
            </span>
            <input
              value={user.public_url}
              onChange={(e) => onPatch({ public_url: e.target.value })}
              placeholder="https://your-domain.example.com"
              disabled={!!urlLock}
              spellCheck={false}
            />
          </label>
        </FormRow>
      </div>

      <div className="divider" />

      <UpdatesSection />

      {browsing && (
        <DirBrowser
          start={user.default_working_dir}
          onPick={(path) => {
            onPatch({ default_working_dir: path });
            setBrowsing(false);
          }}
          onClose={() => setBrowsing(false)}
        />
      )}
    </>
  );
}

/**
 * Update behaviour is stored locally, not in user_settings: it describes this
 * install rather than this user, and a .deb install cannot honour the same
 * choice an AppImage can.
 */
function UpdatesSection() {
  const host = useHostInfo();
  const [pref, setPref] = useState<UpdatePref>(loadUpdatePref);

  const change = (value: UpdatePref) => {
    setPref(value);
    saveUpdatePref(value);
  };

  const managed = host !== undefined && !host.can_self_update;

  return (
    <div className="formsec">
      <div className="formsec__title">Updates</div>

      {managed ? (
        <FormRow
          label="Managed by"
          help="This copy was installed from a system package, so your package manager owns updates. Agento will still tell you when a new version is out."
        >
          <span className="badge">
            {host?.install_kind === "package" ? "System package manager" : "External"}
          </span>
        </FormRow>
      ) : null}

      <FormRow
        label="When an update is available"
        help={UPDATE_PREF_OPTIONS.find((o) => o.value === pref)?.help}
      >
        <div className="col" style={{ gap: "var(--sp-3)" }}>
          {UPDATE_PREF_OPTIONS.filter(
            // "Install automatically" is not offerable when the app cannot
            // replace itself; showing it would promise something we can't do.
            (o) => !(managed && o.value === "auto")
          ).map((o) => (
            <label
              key={o.value}
              className="row"
              style={{ gap: "var(--sp-4)", cursor: "default" }}
            >
              <Checkbox on={pref === o.value} onChange={() => change(o.value)} />
              <span>{o.label}</span>
            </label>
          ))}
        </div>
      </FormRow>
    </div>
  );
}

function lockHelp(envVar: string): string {
  return `Locked by ${envVar}. Change the environment variable and restart Agento — saving a different value here is rejected.`;
}

/* --- Directory browser --------------------------------------------------- */

function DirBrowser({
  start,
  onPick,
  onClose,
}: {
  start: string;
  onPick(path: string): void;
  onClose(): void;
}) {
  const [path, setPath] = useState(start || "~");
  const [creating, setCreating] = useState(false);
  const [newName, setNewName] = useState("");
  const [error, setError] = useState<string>();

  const listing = useResource(
    (signal) => api.get<FSListing>(`/fs${qs({ path })}`, signal),
    [path]
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const here = listing.data?.path ?? path;
  const entries = listing.data?.entries ?? [];

  async function mkdir() {
    const name = newName.trim();
    if (!name) return;
    setError(undefined);
    try {
      await api.post("/fs/mkdir", { path: `${here}/${name}` });
      setNewName("");
      setCreating(false);
      listing.reload();
    } catch (err) {
      setError(describeError(err));
    }
  }

  return (
    <div className="overlay" onMouseDown={onClose}>
      <div className="browser" onMouseDown={(e) => e.stopPropagation()}>
        <div className="browser__head">
          <button
            className="iconbtn"
            onClick={() => listing.data && setPath(listing.data.parent)}
            disabled={!listing.data || listing.data.parent === here}
            title="Parent directory"
          >
            <Icon name="arrowUp" size={14} />
          </button>
          <div className="browser__path" title={here}>
            {tildePath(here)}
          </div>
          <button className="iconbtn" onClick={listing.reload} title="Refresh">
            <Icon name="refresh" size={13} />
          </button>
        </div>

        <div className="browser__list scroll">
          {listing.error ? (
            <div className="msgline msgline--error" style={{ margin: "var(--sp-4)" }}>
              {listing.error}
            </div>
          ) : entries.length === 0 ? (
            <div
              style={{
                padding: "var(--sp-7)",
                textAlign: "center",
                fontSize: "var(--text-sm)",
                color: "var(--fg-tertiary)",
              }}
            >
              {listing.loading ? "Loading…" : "No sub-directories."}
            </div>
          ) : (
            entries.map((e) => (
              <button
                key={e.path}
                className="browser__row"
                onDoubleClick={() => setPath(e.path)}
                onClick={() => setPath(e.path)}
              >
                <Icon name="folder" size={14} />
                <span>{e.name}</span>
              </button>
            ))
          )}
        </div>

        {error && (
          <div className="msgline msgline--error" style={{ margin: "var(--sp-4)" }}>
            {error}
          </div>
        )}

        <div className="browser__foot">
          {creating ? (
            <>
              <label className="field field--sm" style={{ flex: 1 }}>
                <input
                  autoFocus
                  value={newName}
                  onChange={(ev) => setNewName(ev.target.value)}
                  onKeyDown={(ev) => ev.key === "Enter" && mkdir()}
                  placeholder="Folder name"
                  spellCheck={false}
                />
              </label>
              <button className="btn" onClick={mkdir}>
                Create
              </button>
              <button className="btn btn--ghost" onClick={() => setCreating(false)}>
                Cancel
              </button>
            </>
          ) : (
            <>
              <button className="btn" onClick={() => setCreating(true)}>
                <Icon name="plus" size={13} />
                New folder
              </button>
              <div className="spacer" />
              <button className="btn" onClick={onClose}>
                Cancel
              </button>
              <button className="btn btn--primary" onClick={() => onPick(here)}>
                Choose
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

/* --- Claude -------------------------------------------------------------- */

function ClaudePane({
  user,
  locked,
  onPatch,
}: {
  user: UserSettings;
  locked: Record<string, string>;
  onPatch(patch: Partial<UserSettings>): void;
}) {
  const [newDir, setNewDir] = useState("");
  const runLock = locked.claude_config_dir;

  // Older servers do not expose this route at all; a 404 means "no
  // suggestions", not an error worth showing.
  const dirs = useResource(async (signal) => {
    try {
      return await api.get<ClaudeConfigDirs>("/settings/claude-config-dirs", signal);
    } catch (err) {
      if (err instanceof ApiError && err.status === 404) return null;
      throw err;
    }
  }, []);

  const indexed = user.claude_config_dirs ?? [];
  const candidates = (dirs.data?.candidates ?? []).filter(
    (c) => !indexed.includes(c)
  );

  function addDir(path: string) {
    const value = path.trim();
    if (!value || indexed.includes(value)) return;
    onPatch({ claude_config_dirs: [...indexed, value] });
    setNewDir("");
  }

  return (
    <>
      <div className="formsec">
        <div className="formsec__title">Config directory</div>
        <FormRow
          label="Run directory"
          help={
            runLock
              ? lockHelp(runLock)
              : "Passed to Claude Code as CLAUDE_CONFIG_DIR. Blank uses the default."
          }
        >
          <label className={`field ${runLock ? "field--locked" : ""}`}>
            <input
              value={user.claude_config_dir ?? ""}
              onChange={(e) => onPatch({ claude_config_dir: e.target.value })}
              placeholder={dirs.data?.default ?? "~/.claude"}
              className="mono"
              disabled={!!runLock}
              spellCheck={false}
            />
          </label>
        </FormRow>

        <FormRow
          label="Indexed directories"
          help="Extra config directories whose sessions are scanned. The default directory is always indexed."
        >
          <div className="chips">
            {indexed.length === 0 && (
              <span style={{ fontSize: "var(--text-sm)", color: "var(--fg-tertiary)" }}>
                Default directory only.
              </span>
            )}
            {indexed.map((d) => (
              <span key={d} className="chip" title={d}>
                <span className="chip__label mono">{tildePath(d)}</span>
                <button
                  className="chip__x"
                  onClick={() =>
                    onPatch({
                      claude_config_dirs: indexed.filter((x) => x !== d),
                    })
                  }
                  title="Stop indexing"
                >
                  <Icon name="close" size={10} />
                </button>
              </span>
            ))}
          </div>
          <div className="row" style={{ gap: "var(--sp-3)" }}>
            <label className="field field--sm" style={{ flex: 1 }}>
              <input
                value={newDir}
                onChange={(e) => setNewDir(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && addDir(newDir)}
                placeholder="/absolute/path/to/.claude"
                className="mono"
                spellCheck={false}
              />
            </label>
            <button className="btn" onClick={() => addDir(newDir)} disabled={!newDir.trim()}>
              Add
            </button>
          </div>
          {candidates.length > 0 && (
            <div className="chips">
              <span
                style={{
                  fontSize: "var(--text-sm)",
                  color: "var(--fg-tertiary)",
                  alignSelf: "center",
                }}
              >
                Found nearby:
              </span>
              {candidates.map((c) => (
                <button key={c} className="chip chip--add" onClick={() => addDir(c)} title={c}>
                  <Icon name="plus" size={10} />
                  <span className="chip__label mono">{tildePath(c)}</span>
                </button>
              ))}
            </div>
          )}
        </FormRow>
      </div>

      <div className="divider" />

      <ProfilesSection />
    </>
  );
}

function ProfilesSection() {
  const profiles = useResource(
    (signal) => api.get<ClaudeSettingsProfile[] | null>("/claude-settings/profiles", signal),
    []
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [newName, setNewName] = useState("");
  const [adding, setAdding] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState<string>();
  const [renaming, setRenaming] = useState<string>();
  const [renameValue, setRenameValue] = useState("");

  const list = profiles.data ?? [];

  async function run(action: () => Promise<unknown>) {
    setBusy(true);
    setError(undefined);
    try {
      await action();
      profiles.reload();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="formsec">
      <div className="formsec__title">Settings profiles</div>
      <FormRow
        label="Profiles"
        help="Each profile is a separate ~/.claude settings file. Chats and tasks pick one to run under."
      >
        <div className="grouplist">
          {list.length === 0 && (
            <div className="grouplist__row">
              <span style={{ color: "var(--fg-tertiary)" }}>
                {profiles.loading ? "Loading…" : "No profiles."}
              </span>
            </div>
          )}
          {list.map((p) => (
            <div key={p.id} className="grouplist__row">
              {renaming === p.id ? (
                <>
                  <label className="field field--sm" style={{ flex: 1 }}>
                    <input
                      autoFocus
                      value={renameValue}
                      onChange={(e) => setRenameValue(e.target.value)}
                      spellCheck={false}
                    />
                  </label>
                  <button
                    className="btn"
                    disabled={busy || !renameValue.trim()}
                    onClick={() =>
                      run(async () => {
                        await api.put(`/claude-settings/profiles/${p.id}`, {
                          name: renameValue.trim(),
                        });
                        setRenaming(undefined);
                      })
                    }
                  >
                    Save
                  </button>
                  <button className="btn btn--ghost" onClick={() => setRenaming(undefined)}>
                    Cancel
                  </button>
                </>
              ) : confirmDelete === p.id ? (
                <>
                  <span className="confirm" style={{ flex: 1 }}>
                    Delete “{p.name}”? The settings file is removed.
                  </span>
                  <button className="btn btn--ghost" onClick={() => setConfirmDelete(undefined)}>
                    Cancel
                  </button>
                  <button
                    className="btn btn--danger"
                    disabled={busy}
                    onClick={() =>
                      run(async () => {
                        await api.del(`/claude-settings/profiles/${p.id}`);
                        setConfirmDelete(undefined);
                      })
                    }
                  >
                    Delete
                  </button>
                </>
              ) : (
                <>
                  <div className="col" style={{ flex: 1, gap: 1, minWidth: 0 }}>
                    <span style={{ fontWeight: 500 }}>{p.name}</span>
                    <span
                      className="mono truncate"
                      style={{ fontSize: "var(--text-xs)", color: "var(--fg-tertiary)" }}
                      title={p.file_path}
                    >
                      {tildePath(p.file_path)}
                    </span>
                  </div>
                  {p.is_default ? (
                    <span className="badge badge--green">Default</span>
                  ) : (
                    <button
                      className="btn"
                      disabled={busy}
                      onClick={() =>
                        run(() => api.put(`/claude-settings/profiles/${p.id}/default`))
                      }
                    >
                      Make default
                    </button>
                  )}
                  <button
                    className="iconbtn"
                    title="Rename"
                    onClick={() => {
                      setRenameValue(p.name);
                      setRenaming(p.id);
                    }}
                  >
                    <Icon name="edit" size={13} />
                  </button>
                  <button
                    className="iconbtn"
                    title="Duplicate"
                    disabled={busy}
                    onClick={() =>
                      run(() => api.post(`/claude-settings/profiles/${p.id}/duplicate`))
                    }
                  >
                    <Icon name="copy" size={13} />
                  </button>
                  <button
                    className="iconbtn"
                    title="Delete"
                    disabled={p.is_default}
                    onClick={() => setConfirmDelete(p.id)}
                  >
                    <Icon name="trash" size={13} />
                  </button>
                </>
              )}
            </div>
          ))}
        </div>

        {adding ? (
          <div className="row" style={{ gap: "var(--sp-3)" }}>
            <label className="field field--sm" style={{ flex: 1 }}>
              <input
                autoFocus
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                placeholder="Profile name"
                spellCheck={false}
              />
            </label>
            <button
              className="btn"
              disabled={busy || !newName.trim()}
              onClick={() =>
                run(async () => {
                  await api.post("/claude-settings/profiles", { name: newName.trim() });
                  setNewName("");
                  setAdding(false);
                })
              }
            >
              Create
            </button>
            <button className="btn btn--ghost" onClick={() => setAdding(false)}>
              Cancel
            </button>
          </div>
        ) : (
          <button className="btn" style={{ alignSelf: "flex-start" }} onClick={() => setAdding(true)}>
            <Icon name="plus" size={13} />
            New profile
          </button>
        )}

        {error && <div className="msgline msgline--error">{error}</div>}
      </FormRow>
    </div>
  );
}

/* --- Appearance ---------------------------------------------------------- */

function AppearancePane({
  user,
  theme,
  onThemeChange,
  onPatch,
}: {
  user: UserSettings;
  theme: "light" | "dark" | "system";
  onThemeChange(t: "light" | "dark" | "system"): void;
  onPatch(patch: Partial<UserSettings>): void;
}) {
  return (
    <div className="formsec">
      <div className="formsec__title">Theme</div>
      <FormRow label="Appearance" help="Applied immediately; not stored on the server.">
        <div className="row" style={{ gap: "var(--sp-5)" }}>
          {(["light", "dark", "system"] as const).map((t) => (
            <button
              key={t}
              onClick={() => onThemeChange(t)}
              className="col"
              style={{ gap: "var(--sp-3)", alignItems: "center" }}
            >
              <div
                style={{
                  width: 76,
                  height: 50,
                  borderRadius: "var(--r-md)",
                  overflow: "hidden",
                  display: "flex",
                  boxShadow:
                    theme === t
                      ? "0 0 0 2px var(--accent), 0 0 0 4px var(--accent-soft)"
                      : "0 0 0 1px var(--line-strong)",
                }}
              >
                <div
                  style={{
                    width: "34%",
                    background:
                      t === "dark" ? "#202022" : t === "light" ? "#f2f2f3" : "#202022",
                  }}
                />
                <div
                  style={{
                    flex: 1,
                    background:
                      t === "dark" ? "#262628" : t === "light" ? "#ffffff" : "#ffffff",
                  }}
                />
              </div>
              <span
                style={{
                  fontSize: "var(--text-sm)",
                  textTransform: "capitalize",
                  color: theme === t ? "var(--fg)" : "var(--fg-secondary)",
                }}
              >
                {t}
              </span>
            </button>
          ))}
        </div>
      </FormRow>

      <FormRow label="Font size" help="Stored with your account. 0 keeps the system size.">
        <select
          className="nselect"
          style={{ maxWidth: 160 }}
          value={String(user.appearance_font_size)}
          onChange={(e) =>
            onPatch({ appearance_font_size: Number(e.target.value) })
          }
        >
          <option value="0">System default</option>
          {[12, 13, 14, 15, 16, 17, 18].map((n) => (
            <option key={n} value={String(n)}>
              {n} px
            </option>
          ))}
        </select>
      </FormRow>

      <FormRow label="Font family" help="Blank uses the system UI font.">
        <label className="field">
          <input
            value={user.appearance_font_family}
            onChange={(e) => onPatch({ appearance_font_family: e.target.value })}
            placeholder="System UI"
            spellCheck={false}
          />
        </label>
      </FormRow>
    </div>
  );
}

/* --- Notifications ------------------------------------------------------- */

function NotificationsPane({
  editable,
  passwordEdit,
  onPasswordEdit,
}: {
  editable: Editable<NotificationSettings>;
  passwordEdit: string | null;
  onPasswordEdit(v: string | null): void;
}) {
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{ ok: boolean; text: string }>();

  const log = useResource(
    (signal) =>
      api.get<NotificationLogEntry[] | null>(
        `/notifications/log${qs({ limit: 25 })}`,
        signal
      ),
    []
  );

  const draft = editable.draft;
  if (!draft) {
    return (
      <Empty
        icon="bell"
        title={editable.error ? "Notifications unavailable" : "Loading"}
        text={editable.error ?? "Reading notification settings."}
      />
    );
  }

  const provider = draft.provider;
  const prefs = draft.preferences.scheduled_tasks;
  // The server masks a stored password rather than sending it; the field is
  // therefore never populated from the response.
  const hasStoredPassword =
    (editable.server?.provider.password ?? "") !== "" && passwordEdit === null;

  const patchProvider = (patch: Partial<SMTPConfig>) =>
    editable.setDraft((prev) =>
      prev ? { ...prev, provider: { ...prev.provider, ...patch } } : prev
    );

  const patchPrefs = (patch: { on_finished?: boolean; on_failed?: boolean }) =>
    editable.setDraft((prev) =>
      prev
        ? {
            ...prev,
            preferences: {
              ...prev.preferences,
              scheduled_tasks: { ...prev.preferences.scheduled_tasks, ...patch },
            },
          }
        : prev
    );

  async function test() {
    setTesting(true);
    setTestResult(undefined);
    try {
      await api.post("/notifications/test");
      setTestResult({ ok: true, text: "Test email sent using the saved settings." });
    } catch (err) {
      setTestResult({ ok: false, text: describeError(err) });
    } finally {
      setTesting(false);
      log.reload();
    }
  }

  const entries = log.data ?? [];

  return (
    <>
      <div className="formsec">
        <div className="formsec__title">Email notifications</div>
        <div className="grouplist">
          <div className="grouplist__row">
            <div className="col" style={{ flex: 1, gap: 1 }}>
              <span style={{ fontWeight: 500 }}>Send notifications</span>
              <span style={{ fontSize: "var(--text-sm)", color: "var(--fg-tertiary)" }}>
                Delivered over SMTP using the provider below.
              </span>
            </div>
            <Switch
              on={draft.enabled}
              onChange={(v) =>
                editable.setDraft((prev) => (prev ? { ...prev, enabled: v } : prev))
              }
            />
          </div>
        </div>

        <FormRow label="SMTP host">
          <div className="row" style={{ gap: "var(--sp-3)" }}>
            <label className="field" style={{ flex: 1 }}>
              <input
                value={provider.host}
                onChange={(e) => patchProvider({ host: e.target.value })}
                placeholder="smtp.example.com"
                spellCheck={false}
              />
            </label>
            <label className="field" style={{ width: 96 }}>
              <input
                type="number"
                className="tnum"
                value={String(provider.port)}
                onChange={(e) => patchProvider({ port: Number(e.target.value) || 0 })}
                placeholder="587"
              />
            </label>
          </div>
        </FormRow>

        <FormRow label="Encryption">
          <select
            className="nselect"
            style={{ maxWidth: 180 }}
            value={provider.encryption || "none"}
            onChange={(e) => patchProvider({ encryption: e.target.value })}
          >
            <option value="none">None</option>
            <option value="starttls">STARTTLS</option>
            <option value="ssl_tls">SSL / TLS</option>
          </select>
        </FormRow>

        <FormRow label="Username">
          <label className="field">
            <input
              value={provider.username}
              onChange={(e) => patchProvider({ username: e.target.value })}
              autoComplete="off"
              spellCheck={false}
            />
          </label>
        </FormRow>

        <FormRow
          label="Password"
          help={
            hasStoredPassword
              ? "A password is stored. Leave blank to keep it."
              : "Stored on the server and never sent back to this window."
          }
        >
          <label className="field">
            <input
              type="password"
              value={passwordEdit ?? ""}
              onChange={(e) => onPasswordEdit(e.target.value)}
              placeholder={hasStoredPassword ? "•••••••• (unchanged)" : ""}
              autoComplete="new-password"
            />
          </label>
          {passwordEdit !== null && (
            <button
              className="btn"
              style={{ alignSelf: "flex-start" }}
              onClick={() => onPasswordEdit(null)}
            >
              Keep the stored password
            </button>
          )}
        </FormRow>

        <FormRow label="From address">
          <label className="field">
            <input
              value={provider.from_address}
              onChange={(e) => patchProvider({ from_address: e.target.value })}
              placeholder="agento@example.com"
              spellCheck={false}
            />
          </label>
        </FormRow>

        <FormRow label="To addresses" help="Comma-separated.">
          <label className="field">
            <input
              value={provider.to_addresses}
              onChange={(e) => patchProvider({ to_addresses: e.target.value })}
              placeholder="me@example.com, ops@example.com"
              spellCheck={false}
            />
          </label>
        </FormRow>

        <FormRow label="Send a test" help="Uses the settings already saved, not unsaved edits.">
          <button
            className="btn btn--lg"
            style={{ alignSelf: "flex-start" }}
            onClick={test}
            disabled={testing}
          >
            <Icon name="send" size={14} />
            {testing ? "Sending…" : "Send test email"}
          </button>
          {testResult && (
            <div className={`msgline ${testResult.ok ? "msgline--ok" : "msgline--error"}`}>
              {testResult.text}
            </div>
          )}
        </FormRow>
      </div>

      <div className="divider" />

      <div className="formsec">
        <div className="formsec__title">Events</div>
        <div className="grouplist">
          <PrefRow
            label="A scheduled task finishes"
            // Absent means "on" server-side, so an unset preference renders on.
            on={prefs.on_finished !== false}
            onChange={(v) => patchPrefs({ on_finished: v })}
          />
          <PrefRow
            label="A scheduled task fails"
            on={prefs.on_failed !== false}
            onChange={(v) => patchPrefs({ on_failed: v })}
          />
        </div>
      </div>

      <div className="divider" />

      <div className="formsec">
        <div className="row">
          <div className="formsec__title" style={{ flex: 1 }}>
            Delivery log
          </div>
          <button className="btn" onClick={log.reload}>
            <Icon name="refresh" size={13} />
            Refresh
          </button>
        </div>
        {entries.length === 0 ? (
          <div className="msgline">
            {log.loading ? "Loading…" : "Nothing has been sent yet."}
          </div>
        ) : (
          <table className="kvtable">
            <thead>
              <tr>
                <th>When</th>
                <th>Event</th>
                <th>Subject</th>
                <th>Status</th>
              </tr>
            </thead>
            <tbody>
              {entries.map((e) => (
                <tr key={e.id}>
                  <td title={dateTime(e.created_at)} style={{ whiteSpace: "nowrap" }}>
                    {relativeTime(e.created_at)}
                  </td>
                  <td className="mono" style={{ fontSize: "var(--text-xs)" }}>
                    {e.event_type}
                  </td>
                  <td className="truncate" title={e.subject}>
                    {e.subject || "—"}
                  </td>
                  <td>
                    <span
                      className={`badge ${e.status === "sent" ? "badge--green" : "badge--red"}`}
                      title={e.error_msg || undefined}
                    >
                      {e.status}
                    </span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </>
  );
}

function PrefRow({
  label,
  on,
  onChange,
}: {
  label: string;
  on: boolean;
  onChange(v: boolean): void;
}) {
  return (
    <div className="grouplist__row">
      <span style={{ flex: 1 }}>{label}</span>
      <Switch on={on} onChange={onChange} />
    </div>
  );
}

/* --- Data ---------------------------------------------------------------- */

function DataPane({
  user,
  onPatch,
  idleGapError,
}: {
  user: UserSettings;
  onPatch(patch: Partial<UserSettings>): void;
  idleGapError: string | undefined;
}) {
  const [newProject, setNewProject] = useState("");
  const hidden = user.hidden_projects ?? [];

  return (
    <>
      <div className="formsec">
        <div className="formsec__title">Sessions</div>
        <FormRow
          label="Idle gap threshold"
          help={`A pause longer than this ends a session's active stretch. 0 uses the built-in default; otherwise ${IDLE_GAP_MIN}–${IDLE_GAP_MAX} minutes. Changing it re-scans stored transcripts.`}
        >
          <div className="row" style={{ gap: "var(--sp-3)", alignItems: "center" }}>
            <label className="field" style={{ width: 110 }}>
              <input
                type="number"
                className="tnum"
                min={0}
                max={IDLE_GAP_MAX}
                value={String(user.idle_gap_threshold_minutes)}
                onChange={(e) =>
                  onPatch({ idle_gap_threshold_minutes: Number(e.target.value) || 0 })
                }
              />
            </label>
            <span style={{ color: "var(--fg-tertiary)", fontSize: "var(--text-sm)" }}>
              minutes
            </span>
          </div>
          {idleGapError && <div className="msgline msgline--error">{idleGapError}</div>}
        </FormRow>
      </div>

      <div className="divider" />

      <div className="formsec">
        <div className="formsec__title">Hidden projects</div>
        <FormRow
          label="Hidden"
          help="Hidden projects are filtered out of sessions and analytics. Nothing is deleted."
        >
          <div className="chips">
            {hidden.length === 0 && (
              <span style={{ fontSize: "var(--text-sm)", color: "var(--fg-tertiary)" }}>
                Nothing is hidden.
              </span>
            )}
            {hidden.map((p) => (
              <span key={p} className="chip" title={p}>
                <span className="chip__label mono">{tildePath(p)}</span>
                <button
                  className="chip__x"
                  onClick={() =>
                    onPatch({ hidden_projects: hidden.filter((x) => x !== p) })
                  }
                  title="Unhide"
                >
                  <Icon name="close" size={10} />
                </button>
              </span>
            ))}
          </div>
          <div className="row" style={{ gap: "var(--sp-3)" }}>
            <label className="field field--sm" style={{ flex: 1 }}>
              <input
                value={newProject}
                onChange={(e) => setNewProject(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key !== "Enter") return;
                  const v = newProject.trim();
                  if (!v || hidden.includes(v)) return;
                  onPatch({ hidden_projects: [...hidden, v] });
                  setNewProject("");
                }}
                placeholder="Project path to hide"
                className="mono"
                spellCheck={false}
              />
            </label>
            <button
              className="btn"
              disabled={!newProject.trim() || hidden.includes(newProject.trim())}
              onClick={() => {
                onPatch({ hidden_projects: [...hidden, newProject.trim()] });
                setNewProject("");
              }}
            >
              Hide
            </button>
          </div>
        </FormRow>
      </div>
    </>
  );
}

/* --- Pricing ------------------------------------------------------------- */

function PricingPane() {
  const catalog = useResource(
    (signal) => api.get<PricingCatalog>("/pricing/catalog", signal),
    []
  );

  const rows = useMemo(() => {
    const models = catalog.data?.models ?? [];
    return models
      .filter((m) => m.current !== null)
      .sort((a, b) =>
        (a.provider || "~").localeCompare(b.provider || "~") ||
        a.display_name.localeCompare(b.display_name)
      );
  }, [catalog.data]);

  if (catalog.loading && !catalog.data) {
    return <Empty icon="dollar" title="Loading" text="Reading the pricing catalog." />;
  }
  if (catalog.error && !catalog.data) {
    return <Empty icon="alert" title="Pricing unavailable" text={catalog.error} />;
  }

  const unpriced = catalog.data?.unpriced_models ?? [];

  return (
    <div className="formsec">
      <div className="row">
        <div className="formsec__title" style={{ flex: 1 }}>
          Model rates
        </div>
        <span className="badge">revision {catalog.data?.revision ?? 0}</span>
      </div>
      <div className="formrow__help">
        Rates are US dollars per million tokens and drive every cost shown in the
        app. Read-only here — corrections are made through the pricing API.
      </div>

      <div style={{ overflowX: "auto" }}>
        <table className="kvtable">
          <thead>
            <tr>
              <th>Model</th>
              <th>Provider</th>
              <th className="num">Input</th>
              <th className="num">Output</th>
              <th className="num">Cache write</th>
              <th className="num">Cache read</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((m) => {
              const r = m.current;
              if (!r) return null;
              return (
                <tr key={`${m.provider}/${m.model_pattern}`}>
                  <td>
                    <div className="col" style={{ gap: 1 }}>
                      <span>{m.display_name}</span>
                      <span
                        className="mono"
                        style={{ fontSize: "var(--text-xs)", color: "var(--fg-tertiary)" }}
                      >
                        {m.model_pattern}
                        {m.match_type === "prefix" ? "*" : ""}
                      </span>
                    </div>
                    <div style={{ display: "flex", gap: 4, marginTop: 4 }}>
                      {!r.billable && <span className="badge">not billed</span>}
                      {r.estimated && <span className="badge badge--amber">estimated</span>}
                      {r.user_modified && <span className="badge badge--purple">edited</span>}
                      {r.tiers && r.tiers.length > 0 && (
                        <span className="badge">{r.tiers.length} tiers</span>
                      )}
                    </div>
                  </td>
                  <td style={{ color: "var(--fg-secondary)" }}>{r.provider || "—"}</td>
                  <td className="num tnum">{usd(r.input_per_mtok)}</td>
                  <td className="num tnum">{usd(r.output_per_mtok)}</td>
                  <td className="num tnum">{usd(r.cache_write_5m_per_mtok)}</td>
                  <td className="num tnum">{usd(r.cache_read_per_mtok)}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>

      {unpriced.length > 0 && (
        <div className="msgline msgline--warn">
          <span className="msgline__icon">
            <Icon name="alert" size={13} />
          </span>
          <span>
            No rate for {unpriced.join(", ")} — tokens from these models are counted
            but not costed.
          </span>
        </div>
      )}
    </div>
  );
}

/* --- Advanced ------------------------------------------------------------ */

function AdvancedPane({
  user,
  onPatch,
  monitoring,
}: {
  user: UserSettings;
  onPatch(patch: Partial<UserSettings>): void;
  monitoring: Resource<MonitoringEnvelope>;
}) {
  const mon = monitoring.data?.settings;
  const locked = monitoring.data?.locked ?? {};

  return (
    <>
      <div className="formsec">
        <div className="formsec__title">Runtime</div>
        <FormRow
          label="Event worker pool"
          help="Workers draining the internal event bus. 0 uses the built-in default."
        >
          <label className="field" style={{ width: 110 }}>
            <input
              type="number"
              className="tnum"
              min={0}
              value={String(user.event_bus_worker_pool_size)}
              onChange={(e) =>
                onPatch({ event_bus_worker_pool_size: Number(e.target.value) || 0 })
              }
            />
          </label>
        </FormRow>
      </div>

      <div className="divider" />

      <MonitoringSection mon={mon} locked={locked} error={monitoring.error} />

      <div className="divider" />

      <VersionSection />
    </>
  );
}

/**
 * Telemetry, read-only.
 *
 * The desktop app exports no OpenTelemetry and no Prometheus — that is a
 * decision the port records rather than a gap it is working through, and
 * `PUT /api/monitoring` answers 501 to match. Editable controls here would be a
 * save that changes nothing, which is worse than no controls at all.
 *
 * The stored configuration is still shown, for two reasons: an `agento web`
 * sharing this data dir reads the same `monitoring.json`, and `locked` reports
 * which `OTEL_*` variables have pinned a field — which is the kind of thing
 * someone debugging a missing trace comes here to find out.
 */
function MonitoringSection({
  mon,
  locked,
  error,
}: {
  mon?: MonitoringConfig;
  locked: Record<string, string>;
  error?: string;
}) {
  const lockedFields = Object.entries(locked);

  return (
    <div className="formsec">
      <div className="formsec__title">Monitoring</div>

      <div className="msgline">
        <span className="msgline__icon">
          <Icon name="info" size={13} />
        </span>
        <span>
          This app exports no telemetry. The settings below are what{" "}
          <span className="mono">monitoring.json</span> holds — run the Agento
          server if you need OpenTelemetry or Prometheus.
        </span>
      </div>

      {error && !mon && <div className="msgline msgline--error">{error}</div>}

      {mon && (
        <>
          <FormRow label="Export telemetry">
            <span className="mono selectable">{mon.enabled ? "on" : "off"}</span>
          </FormRow>
          <FormRow label="Metrics exporter">
            <span className="mono selectable">{mon.metrics_exporter || "none"}</span>
          </FormRow>
          <FormRow label="Logs exporter">
            <span className="mono selectable">{mon.logs_exporter || "none"}</span>
          </FormRow>
          <FormRow label="OTLP endpoint">
            <span className="mono selectable">{mon.otlp_endpoint || "—"}</span>
          </FormRow>
          <FormRow label="Insecure">
            <span className="mono selectable">{mon.otlp_insecure ? "yes" : "no"}</span>
          </FormRow>
          <FormRow label="Export interval">
            <span className="mono selectable tnum">
              {mon.metric_export_interval_ms} ms
            </span>
          </FormRow>
          {lockedFields.length > 0 && (
            <FormRow
              label="Pinned by the environment"
              help="These fields come from environment variables, which override the stored file."
            >
              <div className="col" style={{ gap: "var(--sp-1)" }}>
                {lockedFields.map(([field, envVar]) => (
                  <span key={field} className="mono selectable">
                    {field} = ${envVar}
                  </span>
                ))}
              </div>
            </FormRow>
          )}
        </>
      )}
    </div>
  );
}

function VersionSection() {
  const version = useResource(
    (signal) => api.get<VersionInfo>("/version", signal),
    []
  );
  const [checking, setChecking] = useState(false);
  const [check, setCheck] = useState<UpdateCheck>();
  const [error, setError] = useState<string>();

  async function checkForUpdate() {
    setChecking(true);
    setError(undefined);
    try {
      setCheck(await api.get<UpdateCheck>("/version/update-check"));
    } catch (err) {
      setError(describeError(err));
    } finally {
      setChecking(false);
    }
  }

  return (
    <div className="formsec">
      <div className="formsec__title">Version</div>
      <FormRow label="Installed">
        <div className="row" style={{ gap: "var(--sp-4)", alignItems: "center" }}>
          <span className="mono selectable">{version.data?.version ?? "—"}</span>
          {version.data?.commit && (
            <span className="badge mono">{version.data.commit}</span>
          )}
          {version.data?.build_date && (
            <span style={{ fontSize: "var(--text-sm)", color: "var(--fg-tertiary)" }}>
              built {dateTime(version.data.build_date)}
            </span>
          )}
        </div>
      </FormRow>
      <FormRow label="Updates">
        <button
          className="btn btn--lg"
          style={{ alignSelf: "flex-start" }}
          onClick={checkForUpdate}
          disabled={checking}
        >
          <Icon name="refresh" size={14} />
          {checking ? "Checking…" : "Check for updates"}
        </button>
        {error && <div className="msgline msgline--error">{error}</div>}
        {check &&
          (check.update_available ? (
            <div className="msgline msgline--ok">
              <span className="msgline__icon">
                <Icon name="arrowUp" size={13} />
              </span>
              <span>
                Version {check.latest_version} is available.{" "}
                <a href={check.release_url} target="_blank" rel="noreferrer">
                  Release notes
                </a>
              </span>
            </div>
          ) : (
            <div className="msgline">
              <span className="msgline__icon">
                <Icon name="check" size={13} />
              </span>
              <span>{check.current_version} is the latest version.</span>
            </div>
          ))}
      </FormRow>
    </div>
  );
}
