import { useEffect, useRef, useState } from "react";
import { Icon } from "../lib/icons";
import { copyText } from "../lib/clipboard";

/**
 * Copy-to-clipboard with its own result as the feedback.
 *
 * The tick is not decoration: `copyText` can fail (see its note on WebKitGTK
 * permissions), and a button that looks identical whether it worked or not is
 * worse than no button — the user finds out by pasting. So success and failure
 * are drawn differently, and the state is driven by what `copyText` returned.
 */
export function CopyButton({
  text,
  title = "Copy",
  className = "iconbtn",
  size = 12,
  label,
}: {
  /** Read at click time via a function so a streaming message copies whole. */
  text: string | (() => string);
  title?: string;
  className?: string;
  size?: number;
  /** Renders beside the icon; omit for an icon-only button. */
  label?: string;
}) {
  const [state, setState] = useState<"idle" | "done" | "failed">("idle");
  const timer = useRef<number>();

  useEffect(() => () => window.clearTimeout(timer.current), []);

  return (
    <button
      className={`${className} ${state === "done" ? "iconbtn--active" : ""}`}
      title={state === "failed" ? "Could not copy" : title}
      onClick={async (e) => {
        // The transcript's code blocks and messages both sit inside clickable
        // regions in places; copying is never also a selection or a toggle.
        e.stopPropagation();
        const ok = await copyText(typeof text === "function" ? text() : text);
        setState(ok ? "done" : "failed");
        window.clearTimeout(timer.current);
        timer.current = window.setTimeout(() => setState("idle"), 1400);
      }}
    >
      <Icon
        name={state === "done" ? "check" : state === "failed" ? "alert" : "copy"}
        size={size}
      />
      {label && <span>{state === "done" ? "Copied" : label}</span>}
    </button>
  );
}
