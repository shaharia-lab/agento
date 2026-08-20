import { memo, useMemo } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import { CopyButton } from "../../components/CopyButton";
import { openExternal } from "../../lib/tauri";
import "../../styles/markdown.css";

/**
 * Assistant and user prose, rendered as the markdown it has always been.
 *
 * The previous renderer split on ``` and wrapped the rest in `<p>`, so every
 * heading, list, table, link and inline `code` reached the reader as literal
 * `##`, `-` and backticks — which is most of what a coding agent writes.
 *
 * Three things about this are deliberate:
 *
 * - **No `rehype-raw`.** react-markdown drops raw HTML by default, and that
 *   default is the security boundary here: a transcript carries tool output,
 *   web page contents and anything a prompt injection put in front of the
 *   model, and this app has no server-side sanitiser. Markdown *syntax* is the
 *   whole feature; embedded HTML is not.
 * - **Links go through `openExternal`.** `window.open` and `target="_blank"`
 *   do not leave a Tauri webview, so an ordinary anchor would either do
 *   nothing or navigate the app itself away from the UI — the second being
 *   unrecoverable, since there is no address bar to come back with.
 * - **Memoised on the text.** A streaming turn re-renders on every token, and
 *   the parse is over the whole message each time. Memoising means an
 *   unchanged earlier message in a long transcript is not re-parsed because a
 *   later one grew.
 */
export const Markdown = memo(function Markdown({
  text,
  caret,
}: {
  text: string;
  /** The streaming cursor, appended after the last block. */
  caret?: boolean;
}) {
  const components = useMemo<Components>(
    () => ({
      a({ href, children }) {
        return (
          <a
            href={href}
            onClick={(e) => {
              e.preventDefault();
              if (href) void openExternal(href);
            }}
          >
            {children}
          </a>
        );
      },
      // `pre` rather than `code`, because the copy button belongs to the block
      // and `code` is also every inline span. react-markdown puts the `code`
      // element inside `pre` for a fenced block, so the text is one level down.
      pre({ children }) {
        return (
          <div className="md-code">
            <div className="md-code__bar">
              <CopyButton
                text={() => codeTextOf(children)}
                title="Copy code"
                className="iconbtn iconbtn--onsurface"
              />
            </div>
            <pre className="selectable">{children}</pre>
          </div>
        );
      },
      // A markdown table can be far wider than the detail pane, and a pane that
      // scrolls sideways as a whole is unusable. Scroll the table alone.
      table({ children }) {
        return (
          <div className="md-table">
            <table>{children}</table>
          </div>
        );
      },
    }),
    []
  );

  if (!text) return null;

  return (
    <div className="msg__text md">
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={components}>
        {text}
      </ReactMarkdown>
      {caret && <span className="caret" />}
    </div>
  );
});

/**
 * The plain text of a fenced block, for the clipboard.
 *
 * Walks the rendered children rather than re-deriving it from the source: by
 * this point react-markdown has already stripped the fence and its info string,
 * and re-parsing the message to find which block this was would need position
 * data the component does not receive.
 */
function codeTextOf(node: React.ReactNode): string {
  if (node === null || node === undefined || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(codeTextOf).join("");
  if (typeof node === "object" && "props" in node) {
    const props = (node as { props?: { children?: React.ReactNode } }).props;
    return codeTextOf(props?.children);
  }
  return "";
}
