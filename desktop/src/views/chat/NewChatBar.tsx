import { useEffect, useMemo, useRef } from "react";
import { api } from "../../lib/api";
import { useResource } from "../../lib/hooks";
import { Icon } from "../../lib/icons";
import { Dropdown } from "../../components/ui";
import { DirField, useDirPicker } from "../../components/DirField";
import {
  EMPTY_PREFS,
  loadNewChatPrefs,
  type NewChatPrefs,
} from "../../lib/newChatPrefs";
import type {
  Agent,
  ClaudeSettingsProfile,
  SettingsResponse,
} from "../../lib/types";
import "../../styles/newchat.css";

/** What the agent editor offers, so a chat and an agent read the same. */
const MODELS = [
  { value: "sonnet", label: "Sonnet" },
  { value: "opus", label: "Opus" },
  { value: "haiku", label: "Haiku" },
];

/**
 * All four of Claude Code's modes.
 *
 * The *agent* form deliberately offers a narrower set, because an agent's mode
 * is applied to unattended runs where nothing can answer a prompt. A chat
 * always has a human in front of it, so "plan" and "dontAsk" are exactly the
 * modes a conversation wants — and before migration 30 a chat could not express
 * any of them: the interactive permission handler forced "default" on every
 * turn, which is why a `bypass` agent still stopped to ask.
 */
const PERMISSION_MODES = [
  { value: "", label: "Permissions: agent default" },
  { value: "default", label: "Permissions: ask before acting" },
  { value: "bypass", label: "Permissions: never ask" },
  { value: "plan", label: "Permissions: plan only" },
  { value: "dontAsk", label: "Permissions: don't ask" },
];

/**
 * The same five modes without the picker's "Permissions:" prefix, for anywhere
 * that reports a stored mode rather than offering one. Derived from the list
 * above so a mode cannot be offered here and unnamed there.
 */
export const PERMISSION_LABELS: Record<string, string> = Object.fromEntries(
  PERMISSION_MODES.map((m) => [
    m.value,
    m.label.replace(/^Permissions: /, "").replace(/^./, (c) => c.toUpperCase()),
  ])
);

/**
 * The controls a new conversation is configured with, and the resolution of
 * what they start on.
 *
 * Three sources, in order: what was picked last (localStorage), then the
 * server's own defaults (`GET /settings`, plus whichever settings profile is
 * marked default), then nothing. A remembered value that no longer exists — an
 * agent that was deleted, a profile that was renamed — is dropped rather than
 * sent, because the create call would 404 on the first and silently run under
 * the wrong file on the second.
 */
export function NewChatBar({
  agents,
  agentsError,
  value,
  onChange,
}: {
  agents: Agent[];
  agentsError?: string;
  value: NewChatPrefs;
  onChange(next: NewChatPrefs): void;
}) {
  const picker = useDirPicker();

  const settings = useResource<SettingsResponse | null>(
    (signal) => api.get<SettingsResponse>("/settings", signal),
    []
  );
  // A build with no profiles configured answers an empty list; an older server
  // has no such route. Neither is an error worth showing in a compose bar, so
  // both collapse to "no profile picker".
  const profiles = useResource<ClaudeSettingsProfile[] | null>(
    (signal) =>
      api
        .get<ClaudeSettingsProfile[] | null>("/claude-settings/profiles", signal)
        .catch(() => null),
    []
  );

  const profileList = profiles.data ?? [];

  // Resolution runs once per mount, and only over the fields still untouched.
  // Re-running it on every render would fight the user: clearing the working
  // directory to type a new one would immediately refill it with the default.
  const resolved = useRef(false);
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;
  const valueRef = useRef(value);
  valueRef.current = value;

  useEffect(() => {
    if (resolved.current) return;
    // Wait for both, so the two sources are applied as one edit.
    if (settings.loading || profiles.loading) return;
    resolved.current = true;

    const remembered = loadNewChatPrefs();
    const defaults = settings.data?.settings;
    const defaultProfile =
      profileList.find((p) => p.is_default) ?? profileList[0];

    const agentSlug = agents.some((a) => a.slug === remembered.agentSlug)
      ? remembered.agentSlug
      : "";
    const settingsProfileId = profileList.some(
      (p) => p.id === remembered.settingsProfileId
    )
      ? remembered.settingsProfileId
      : (defaultProfile?.id ?? "");

    onChangeRef.current({
      ...valueRef.current,
      agentSlug,
      workingDir:
        valueRef.current.workingDir ||
        remembered.workingDir ||
        defaults?.default_working_dir ||
        "",
      model: remembered.model || defaults?.default_model || "",
      permissionMode: remembered.permissionMode,
      settingsProfileId,
    });
    // `agents` and `profileList` are read through the loading gate above rather
    // than tracked, so this depends only on whether the two fetches are done.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [settings.loading, profiles.loading]);

  const agent = useMemo(
    () => agents.find((a) => a.slug === value.agentSlug),
    [agents, value.agentSlug]
  );
  // #299: `resolveAgentConfig` branches on whether the chat **names an agent**,
  // and the runner then sets a model only when the agent has one — so a chat's
  // own model is simply not consulted for an agent that carries one. Showing an
  // editable picker there would be a control that silently does nothing.
  const modelLocked = !!agent?.model;

  const set = (patch: Partial<NewChatPrefs>) => onChange({ ...value, ...patch });

  return (
    <>
      <div className="newchat">
        <div className="newchat__row">
          {/* An agent picker with no agents to pick is a control whose only
              option is "no agent", which is also what happens if it is absent.
              The Agents view is where one gets created. */}
          {agents.length > 0 && (
            <Dropdown
              small
              value={value.agentSlug}
              onChange={(agentSlug) => set({ agentSlug })}
              ariaLabel="Agent"
              options={[
                { value: "", label: "No agent — direct chat" },
                ...agents.map((a) => ({
                  value: a.slug,
                  label: a.name || a.slug,
                })),
              ]}
            />
          )}

          <Dropdown
            small
            disabled={modelLocked}
            value={modelLocked ? (agent?.model ?? "") : value.model}
            onChange={(model) => set({ model })}
            ariaLabel="Model"
            label={
              modelLocked
                ? `Model: ${agent?.model} (from agent)`
                : `Model: ${labelFor(MODELS, value.model) || "default"}`
            }
            options={[
              { value: "", label: "Default model" },
              ...withCurrent(MODELS, value.model),
            ]}
          />

          <Dropdown
            small
            value={value.permissionMode}
            onChange={(permissionMode) => set({ permissionMode })}
            ariaLabel="Permission mode"
            options={PERMISSION_MODES}
          />

          {profileList.length > 0 && (
            <Dropdown
              small
              value={value.settingsProfileId}
              onChange={(settingsProfileId) => set({ settingsProfileId })}
              ariaLabel="Claude settings profile"
              label={`Settings: ${
                profileList.find((p) => p.id === value.settingsProfileId)?.name ??
                "default"
              }`}
              options={profileList.map((p) => ({
                value: p.id,
                label: p.is_default ? `${p.name} (default)` : p.name,
              }))}
            />
          )}
        </div>

        <div className="newchat__row">
          <DirField
            compact
            value={value.workingDir}
            onChange={(workingDir) => set({ workingDir })}
            title="Choose working directory"
            placeholder="Working directory (required)"
            browse={picker.browse}
          />
          {agentsError && (
            <span className="newchat__note">
              <Icon name="alert" size={12} />
              Agents unavailable — {agentsError}
            </span>
          )}
        </div>
      </div>
      {picker.browser}
    </>
  );
}

/** The initial value, before the effect above resolves the real defaults. */
export const NEW_CHAT_INITIAL = EMPTY_PREFS;

function labelFor(options: { value: string; label: string }[], value: string) {
  return options.find((o) => o.value === value)?.label ?? value;
}

/** Keep a stored model the picker does not enumerate, e.g. a full model id. */
function withCurrent(
  options: { value: string; label: string }[],
  value: string
) {
  if (!value || options.some((o) => o.value === value)) return options;
  return [{ value, label: value }, ...options];
}
