/* ============================================================================
   The one action strip at the foot of a form (#519).

   Six views submit from a savebar and there were two implementations of it:
   `IntegrationsView`'s `SaveBar` — which #516 made a component precisely so a
   future divergence would be a deliberate edit — and `AgentsView`'s
   `.agents-savebar`, a second strip with its own class family, its own
   message element, its own error variant and `btn--lg` buttons. The class was
   shared and the JSX was copied, and the JSX is what drifted; so the component
   is what has to be shared, not the class.

   **Button size is `btn`, decided once.** Five of the six were already `btn`
   and `.agents-savebar`'s `btn--lg` was the only user of that size anywhere in
   a savebar, so `btn` is both the majority and the direction that leaves the
   other five untouched.

   **The message can carry an icon and an error tone**, which is the only
   capability `.agents-savebar` had and `.savebar` lacked — the reason the fork
   existed at all. Both are opt-in: a savebar passing neither emits exactly the
   markup it emitted before, so nothing about the five pre-existing strips
   moves. `styles/savebar.css` is imported here rather than by a view, the
   `components/charts.tsx` shape, so a section that does not import
   `integrations.css` still gets a styled strip.

   The verbs are `lib/formVerbs.ts`'s and are not overridable — that grammar is
   repo-wide (`CLAUDE.md` → *Conventions*) and a per-view label is how it came
   apart the first time:

   - **The primary verb follows existence**: `Create` while the record does not
     exist yet, `Save` once it does. The in-flight label follows it.
   - **Its partner follows the same split**: `Discard` throws away a record
     that was never stored, `Revert` restores one that was. Two words because
     they undo two different things; a single "Cancel" would claim to undo a
     save.
   - **No `+` icon on a submit.** The `+` marks a list-level *New X* that opens
     a blank thing; on a submit it reads as "add another one".
   ========================================================================== */

import type { ReactNode } from "react";
import { Icon, type IconName } from "../lib/icons";
import { partnerLabel, submitLabel } from "../lib/formVerbs";
import "../styles/savebar.css";

export function SaveBar({
  creating,
  busy,
  canSubmit,
  message,
  messageIcon,
  messageTone,
  extra,
  onDiscard,
  onSubmit,
}: {
  /** Whether the record does not exist yet — decides both button words. */
  creating: boolean;
  /** A request is in flight: neither button is actionable. */
  busy: boolean;
  /** Whether the form is in a state the primary may submit from. */
  canSubmit: boolean;
  /**
   * Why the primary is disabled, or that there are unsaved changes — the
   * strip's own explanation, so a form never leaves the user guessing at a
   * greyed-out button.
   */
  message: string;
  /**
   * An icon beside the message. Opt-in: without it the message renders as a
   * bare string child, which is what the five savebars that predate this
   * component do.
   */
  messageIcon?: IconName;
  /** `error` reddens the message — a failure that is not itself a form state. */
  messageTone?: "error";
  /**
   * An extra button between the partner and the primary. One caller: the
   * gateway's provider form, whose credential check can never go green against
   * a base that serves no model list, so "Save anyway" has to be reachable.
   */
  extra?: ReactNode;
  onDiscard(): void;
  onSubmit(): void;
}) {
  const textClass = [
    "savebar__text",
    messageIcon ? "savebar__text--icon" : "",
    messageTone === "error" ? "savebar__text--error" : "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div className="savebar">
      <span className={textClass}>
        {messageIcon ? (
          <>
            <Icon name={messageIcon} size={13} />
            <span className="truncate">{message}</span>
          </>
        ) : (
          message
        )}
      </span>
      <button className="btn" onClick={onDiscard} disabled={busy}>
        {partnerLabel(creating)}
      </button>
      {extra}
      <button
        className="btn btn--primary"
        onClick={onSubmit}
        disabled={!canSubmit || busy}
      >
        {submitLabel(creating, busy)}
      </button>
    </div>
  );
}
