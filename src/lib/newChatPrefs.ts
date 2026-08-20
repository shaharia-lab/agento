/* ============================================================================
   What the New Chat bar was set to last time.

   These live in localStorage rather than in `user_settings` on purpose. The
   server already stores a *default* working directory and model, and those mean
   "what a chat should start from"; this means "what I picked last", which is a
   different claim and belongs to this install rather than to the account. Two
   consequences follow: an `agento web` on the same data dir is unaffected, and
   clearing site data resets the memory to the server defaults rather than to
   nothing.

   Every field is optional on read, because the shape will grow and a stored
   blob written by an older build must not throw away the fields it does have.
   ========================================================================== */

export interface NewChatPrefs {
  agentSlug: string;
  workingDir: string;
  model: string;
  permissionMode: string;
  settingsProfileId: string;
}

const KEY = "agento.newchat";

export const EMPTY_PREFS: NewChatPrefs = {
  agentSlug: "",
  workingDir: "",
  model: "",
  permissionMode: "",
  settingsProfileId: "",
};

/**
 * The last-used choices, or the empty set.
 *
 * Reads defensively rather than trusting the blob: localStorage is shared with
 * anything else on this origin, survives every upgrade, and a `JSON.parse` of a
 * corrupted value would take the whole view down at first render.
 */
export function loadNewChatPrefs(): NewChatPrefs {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return EMPTY_PREFS;
    const parsed = JSON.parse(raw) as Partial<NewChatPrefs>;
    if (!parsed || typeof parsed !== "object") return EMPTY_PREFS;
    return {
      agentSlug: str(parsed.agentSlug),
      workingDir: str(parsed.workingDir),
      model: str(parsed.model),
      permissionMode: str(parsed.permissionMode),
      settingsProfileId: str(parsed.settingsProfileId),
    };
  } catch {
    return EMPTY_PREFS;
  }
}

/**
 * Remember the choices behind a chat that was actually created.
 *
 * Saving on *create* rather than on every keystroke is the whole design: a
 * half-typed path or an agent the user clicked past is not a preference, and
 * writing one would mean the next chat opens on something nobody chose.
 */
export function saveNewChatPrefs(prefs: NewChatPrefs): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(prefs));
  } catch {
    // A private-mode or quota failure costs the memory, not the chat.
  }
}

function str(value: unknown): string {
  return typeof value === "string" ? value : "";
}
