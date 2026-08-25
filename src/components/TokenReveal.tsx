import { CopyButton } from "./CopyButton";
import { Icon } from "../lib/icons";
import "../styles/security.css";

/**
 * The one-time reveal of a freshly minted API token.
 *
 * Lifted out of `settings/SecurityPane.tsx` when the LLM Gateway Overview (#427)
 * became a second place that mints one. Two copies of this block is exactly the
 * shape that drifts: the invariant it carries — **the token is shown once and
 * nothing stores it** — is a property of where the value lives, and a second
 * implementation is a second chance to put it somewhere durable.
 *
 * So the rules live here, in one place:
 *
 * - The caller holds the token in component state and nowhere else. There is no
 *   prop for a persisted value because there is no persisted value: the
 *   creation response is the only copy that will ever exist, and the list route
 *   answers rows without a `token` field.
 * - The banner has **no timer**. A toast that faded would lose the credential,
 *   so it stays until the user dismisses it.
 * - Dismissing is the caller's `onDismiss`, which must clear that state.
 */
export function TokenReveal({
  name,
  token,
  onDismiss,
  children,
}: {
  /** The token's name, so a user with two banners open can tell them apart. */
  name: string;
  token: string;
  onDismiss(): void;
  /** Extra copy under the token — e.g. what this credential is for. */
  children?: React.ReactNode;
}) {
  return (
    <div className="secnew">
      <div className="secnew__head">
        <Icon name="shield" size={14} />
        <span>
          <strong>{name}</strong> created. Copy it now — this is the only time it
          is shown, and it is not stored anywhere.
        </span>
        <button className="iconbtn" title="Dismiss" onClick={onDismiss}>
          <Icon name="close" size={12} />
        </button>
      </div>
      <div className="secnew__token">
        <code className="mono selectable">{token}</code>
        <CopyButton
          text={token}
          title="Copy token"
          className="btn"
          label="Copy"
        />
      </div>
      {children}
    </div>
  );
}
