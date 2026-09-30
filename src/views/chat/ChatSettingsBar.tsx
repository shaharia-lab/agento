import { Dropdown } from "../../components/ui";
import type { Agent, ChatSession } from "../../lib/types";
import {
  labelFor,
  MODELS,
  PERMISSION_MODES,
  withCurrent,
} from "./NewChatBar";

/**
 * The model and permission mode of an **existing** chat, in the composer strip
 * (#722). Built from `NewChatBar`'s own lists and rules so the two strips offer
 * the same choices under the same words.
 *
 * A change is a `PATCH /chats/{id}` and applies from the next message: the
 * runner reads both columns from the row at the start of every turn. So the
 * controls are disabled while a turn runs rather than queueing a change behind
 * it.
 */
export function ChatSettingsBar({
  session,
  agent,
  disabled,
  onChange,
}: {
  session: ChatSession;
  agent?: Agent;
  disabled?: boolean;
  onChange(body: { model?: string; permission_mode?: string }): void;
}) {
  // #299: the runner consults a chat's own model only when the chat names no
  // agent (`runner.rs`, `build_options`) — an agent with an empty model runs
  // with none at all. So the picker locks on the slug, wider than
  // `NewChatBar`'s rule: a stored model the next turn ignores would be a
  // control that silently does nothing.
  const modelLocked = !!session.agent_slug;
  const model = session.model ?? "";
  const mode = session.permission_mode ?? "";

  return (
    <div className="composer__settings">
      <Dropdown
        small
        disabled={disabled || modelLocked}
        value={modelLocked ? (agent?.model ?? "") : model}
        onChange={(next) => {
          if (next !== model) onChange({ model: next });
        }}
        ariaLabel="Model"
        label={
          modelLocked
            ? `Model: ${agent?.model || "agent default"} (from agent)`
            : `Model: ${labelFor(MODELS, model) || "default"}`
        }
        options={[
          { value: "", label: "Default model" },
          ...withCurrent(MODELS, model),
        ]}
      />
      {/* Stays enabled under an agent: the chat's own mode already beats the
          agent's in the runner. */}
      <Dropdown
        small
        disabled={disabled}
        value={mode}
        onChange={(next) => {
          if (next !== mode) onChange({ permission_mode: next });
        }}
        ariaLabel="Permission mode"
        options={withCurrent(PERMISSION_MODES, mode)}
      />
    </div>
  );
}
