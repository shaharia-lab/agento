import type { ClaudeSessionSummary } from "./types";

/**
 * What Claude Code's `agent-name` event actually carries — and why nothing may
 * treat it as the identity of whoever produced a message.
 *
 * `/rename` is what writes it. Measured across 2,231 transcripts on one
 * developer machine: **every** `agentName` value was a session title, and in 35
 * of the 37 sessions carrying one it was byte-identical to the `custom-title`
 * event written beside it. The two that differed came from Claude Code 2.1.211
 * and 2.1.224, which wrote `agent-name` *alone* for a rename; 2.1.234 writes
 * both, back to back, and re-appends the pair on every resume. So the event's
 * spelling has already moved once across versions, and in none of them did it
 * name an agent.
 *
 * `native/scanner/summary_file.rs` stores it verbatim into
 * `claude_session_cache.agent_name`, and must keep doing so — the column and
 * the wire field are pinned against the Go server, so the fix for a
 * *presentation* mistake cannot live there. The judgement about what the value
 * means belongs here, in the one layer free to change.
 *
 * Two rules follow from that, and they are deliberately not the same rule:
 *
 * - **It is never a message byline.** A byline says who spoke, and this field
 *   has never said that in any version. `SessionDetail` therefore labels
 *   assistant messages with a constant, so no future spelling of `agent-name`
 *   can leak into a per-message label again. That is the defence that does not
 *   depend on guessing what a value looks like — the symptom this fixes was
 *   every message in a renamed session bylined with a 100-character sentence.
 * - **It is shown as session metadata only when it is not already on screen**,
 *   which is what `sessionAgentName` below decides. Suppressing a duplicate is
 *   already the house rule next to it: the inspector shows `AI title` only when
 *   it differs from the resolved `display_title`.
 *
 * The surviving case is the one worth surfacing rather than hiding: a value
 * matching none of the session's titles is a rename Agento's own title
 * resolution cannot see, because those older Claude Code versions never wrote
 * the `custom-title` event that `native_title` is read from. Showing it is how
 * such a session admits it was renamed.
 */
export function sessionAgentName(session: ClaudeSessionSummary): string {
  const name = session.agent_name?.trim() ?? "";
  if (!name) return "";
  // `preview` is in the list because `display_title` falls back to it, so a
  // session with no title event of any kind can still be showing this string.
  const titles = [
    session.custom_title,
    session.native_title,
    session.ai_title,
    session.display_title,
    session.preview,
  ];
  return titles.some((t) => t?.trim() === name) ? "" : name;
}
