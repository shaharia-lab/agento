/**
 * `match_snippet` — the server's answer to "why did this row match?" — and the
 * one place its highlight sentinels are spelled on this side of the wire.
 *
 * The backend wraps every matched term in **U+0001 / U+0002**
 * (`native/search/mod.rs`'s `SNIPPET_MARK_START` / `SNIPPET_MARK_END`, emitted
 * by FTS5's `snippet()` as `char(1)` / `char(2)`). Those two are unambiguous
 * *by construction* rather than by convention: `normalize_text` collapses every
 * control character to a space before a byte is indexed, so neither can occur
 * in the transcript text a snippet is cut from.
 *
 * **They are markers, not markup.** The snippet carries the transcript's own
 * bytes — a session about web development genuinely contains `<script>` — so
 * the only safe renderer is one that splits on the sentinels and hands the
 * segments to React as text. Nothing here produces HTML, and nothing
 * downstream may use `dangerouslySetInnerHTML`: the inertness #438 asks for is
 * a property of never building markup in the first place, not of an escaping
 * pass that could be forgotten one call site later.
 */

export const SNIPPET_MARK_START = "\u0001";
export const SNIPPET_MARK_END = "\u0002";

/* These two are a wire contract with `native/search/mod.rs`, and this repo has
   no TypeScript test harness — `npm run build` is `tsc --noEmit && vite build`
   and that is the whole frontend gate. So they are pinned at the *type* level,
   the way `views/gateway/snippets.ts` pins the gateway's two base URLs (#427):
   respelling either constant fails `tsc`, i.e. CI. Exported so `noUnusedLocals`
   does not flag the guard away. */
type Eq<A, B> =
  (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2
    ? true
    : false;
type Expect<T extends true> = T;
export type PinnedSnippetMarkStart = Expect<
  Eq<typeof SNIPPET_MARK_START, "\u0001">
>;
export type PinnedSnippetMarkEnd = Expect<
  Eq<typeof SNIPPET_MARK_END, "\u0002">
>;

/** One run of snippet text, either inside a highlight or outside every one. */
export interface SnippetPart {
  text: string;
  hit: boolean;
}

/** Neither marker may survive into rendered text, however malformed the input. */
function stripMarkers(s: string): string {
  return s.replace(/[\u0001\u0002]/g, "");
}

/**
 * Split a `match_snippet` into its highlighted and plain runs.
 *
 * Empty runs are dropped, so a snippet opening on a match does not lead with an
 * empty text node. An **unterminated** highlight — a start marker with no end —
 * highlights to the end of the snippet rather than emitting the sentinel: a
 * snippet that highlights too much is a display defect, a visible U+0001 reads
 * as corruption.
 */
export function snippetParts(snippet: string): SnippetPart[] {
  const parts: SnippetPart[] = [];
  const push = (text: string, hit: boolean) => {
    const t = stripMarkers(text);
    if (t) parts.push({ text: t, hit });
  };

  let rest = snippet;
  while (rest) {
    const open = rest.indexOf(SNIPPET_MARK_START);
    if (open < 0) {
      push(rest, false);
      break;
    }
    push(rest.slice(0, open), false);
    rest = rest.slice(open + SNIPPET_MARK_START.length);

    const close = rest.indexOf(SNIPPET_MARK_END);
    if (close < 0) {
      push(rest, true);
      break;
    }
    push(rest.slice(0, close), true);
    rest = rest.slice(close + SNIPPET_MARK_END.length);
  }
  return parts;
}

/**
 * The snippet with its markers removed — for a `title` tooltip, where the whole
 * line is worth reading but the cell has already ellipsised it. Never for HTML.
 */
export function snippetText(snippet: string): string {
  return stripMarkers(snippet);
}
