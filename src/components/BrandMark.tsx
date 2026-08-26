/* ============================================================================
   Provider brand marks (#471).

   The LLM Gateway's Providers list drew the same `database` icon on every row,
   so four upstreams read as four identical grey squares. This resolves a mark
   per row instead — and the interesting part is what it is keyed on.

   **The key is the registrable domain of `base_url`, not the provider type.**
   `type` is one of four *adapter dialects*, so OpenRouter, Groq, Together,
   DeepSeek and Fireworks all carry `type: "openai"` and would all show one
   mark. The base URL is the thing that actually distinguishes them, and it is
   not shown in the list at all.

   **The marks are bundled and cost zero network requests.** The obvious
   alternative — fetch the host's favicon, or ask a third-party favicon
   service — is refused deliberately: `tauri.conf.json`'s CSP is `img-src 'self'
   data: blob: asset: http://asset.localhost`, so a remote `<img>` is blocked
   and the feature would have to *widen the CSP to arbitrary remote origins*
   first. Beyond that it would make a settings pane phone out to third parties
   on render, leaking the user's configured endpoints — private and
   self-hosted hostnames included — to those hosts; and it breaks offline,
   behind a proxy, and on any self-hosted endpoint with no favicon at all.

   **The table is keyed on registrable domains and matched on a label
   boundary**, so `api.openai.com` collapses onto `openai.com` while
   `notopenai.com` does not. It is an allowlist rather than a computation
   because correct eTLD+1 needs a public suffix list (the naive "last two
   labels" rule is wrong for `api.foo.co.uk`), and shipping a PSL for a
   decorative feature is not a trade worth making.

   ## Adding a provider mark

   1. Add the registrable domain to `DOMAIN_MARKS` (no subdomain — the match
      already collapses those).
   2. If it is a new vendor, give it a key in `MARKS`. `MarkKey` is *derived*
      from the two tables, so `MARKS` being `Record<MarkKey, Mark>` makes `tsc`
      fail until you do — that completeness check is the guard here, and it is
      why the tables carry `as const satisfies`.
   3. Leave the fallback chain alone: unknown domain falls back to the type
      mark, and an unknown type falls back to the generic `database` icon. An
      unlisted provider therefore degrades to something sensible and never to
      an empty slot, which is what keeps this table additive and never urgent.

   ## Why every vendor mark is a monogram

   The refinement authorised two forms — a monochrome silhouette for marks
   whose brand guidelines permit redistribution, a lettered monogram for any
   that do not — and asked the implementing change to say which it used. This
   uses **the monogram for every vendor**, and ships no vendor artwork at all.
   Confirming redistribution terms for eleven separate trademark holders is not
   something this change can do, and a hand-drawn approximation of a logo is
   both the legally riskier option and the less legible one at 28px. Swapping
   any one of them for a licensed silhouette later is a one-row edit: give that
   `Mark` an `icon` (or an inline `<svg>`) instead of a `label`.

   Only `selfhosted` is a glyph, and it is the house icon set's own `cpu` —
   nobody's brand.
   ========================================================================== */

import type { Tone } from "../lib/format";
import { Icon, type IconName } from "../lib/icons";
import type { Eq, Expect } from "../lib/typeAssert";
import type { GatewayProviderSummary, GatewayProviderType } from "../lib/types";

/**
 * Registrable domain → mark key. Matched on a label boundary, so every
 * subdomain of a listed domain collapses onto the same mark.
 */
const DOMAIN_MARKS = {
  "openai.com": "openai",
  "anthropic.com": "anthropic",
  "googleapis.com": "gemini",
  "google.com": "gemini",
  "z.ai": "zai",
  "openrouter.ai": "openrouter",
  "groq.com": "groq",
  "together.ai": "together",
  "together.xyz": "together",
  "deepseek.com": "deepseek",
  "mistral.ai": "mistral",
  "x.ai": "xai",
  "fireworks.ai": "fireworks",
} as const satisfies Record<string, string>;

/**
 * The fallback when the base URL names no listed domain — including the empty
 * base URL, which means "this provider's own default endpoint".
 */
const TYPE_MARKS = {
  anthropic: "anthropic",
  openai: "openai",
  gemini: "gemini",
  glm: "zai",
} as const satisfies Record<GatewayProviderType, string>;

/**
 * Derived from the tables rather than declared, so a domain or type mapped to
 * a mark that `MARKS` does not draw fails the build.
 */
export type MarkKey =
  | (typeof DOMAIN_MARKS)[keyof typeof DOMAIN_MARKS]
  | (typeof TYPE_MARKS)[keyof typeof TYPE_MARKS]
  | "selfhosted";

interface Mark {
  /** The monogram drawn in the tile, unless `icon` names a glyph instead. */
  label?: string;
  /** An icon from the house set — used only where no vendor is named. */
  icon?: IconName;
  /** One of `views.css`' existing `.avatar--*` tones. No new CSS. */
  tone: Tone;
  /** The tile's `title`, so the mark is readable as well as scannable. */
  title: string;
}

const MARKS: Record<MarkKey, Mark> = {
  openai: { label: "OA", tone: "green", title: "OpenAI" },
  anthropic: { label: "AN", tone: "amber", title: "Anthropic" },
  gemini: { label: "GE", tone: "accent", title: "Google Gemini" },
  zai: { label: "ZA", tone: "purple", title: "Z.AI" },
  openrouter: { label: "OR", tone: "teal", title: "OpenRouter" },
  groq: { label: "GQ", tone: "red", title: "Groq" },
  together: { label: "TG", tone: "green", title: "Together AI" },
  deepseek: { label: "DS", tone: "accent", title: "DeepSeek" },
  mistral: { label: "MS", tone: "amber", title: "Mistral AI" },
  xai: { label: "XA", tone: "purple", title: "xAI" },
  fireworks: { label: "FW", tone: "red", title: "Fireworks AI" },
  selfhosted: { icon: "cpu", tone: "teal", title: "Self-hosted endpoint" },
};

/**
 * Hosts that mean "this machine". Two spellings to get right: `new URL()`
 * serializes an IPv6 host **bracketed**, so `[::1]` is the only form that can
 * ever reach here and a bare `::1` entry would be dead; and `0.0.0.0` is a
 * bind-any address rather than a loopback one, but it is what a locally-run
 * server's own printed URL often says, so it belongs in this set even though
 * it makes the set wider than its name.
 */
const SELF_HOSTED_HOSTS = new Set(["localhost", "127.0.0.1", "0.0.0.0", "[::1]"]);

/**
 * Suffixes that mean "somebody's own network". Note this is a *suffix* rule on
 * the whole host, so `llm.internal.acme.corp` is **not** self-hosted — it ends
 * in `.corp` — and correctly falls through to the provider's type mark.
 */
const SELF_HOSTED_SUFFIXES = [".local", ".internal", ".lan", ".localdomain"];

const DOMAIN_KEYS = Object.keys(DOMAIN_MARKS) as (keyof typeof DOMAIN_MARKS)[];

/**
 * A stored `base_url` is user-typed and need not parse — and an empty one is
 * the ordinary case, meaning the provider's own default endpoint. Both answer
 * `null`, which is what sends the caller to the type fallback.
 */
function hostOf(baseUrl: string): string | null {
  const raw = baseUrl.trim();
  if (raw === "") return null;
  try {
    return new URL(raw).hostname.toLowerCase();
  } catch {
    return null;
  }
}

function isSelfHosted(host: string): boolean {
  return (
    SELF_HOSTED_HOSTS.has(host) || SELF_HOSTED_SUFFIXES.some((s) => host.endsWith(s))
  );
}

/**
 * `type` is `GatewayProviderType` on the wire, but the column is a string and
 * a newer backend can hold a dialect this build does not know — so the lookup
 * is guarded rather than indexed, and an unknown type answers `null`.
 */
function typeMark(type: GatewayProviderType): MarkKey | null {
  return Object.prototype.hasOwnProperty.call(TYPE_MARKS, type)
    ? TYPE_MARKS[type]
    : null;
}

/**
 * The whole keying rule, exported separately from the component so it can be
 * pinned and reused (`ModelsView` / `UsageView` name providers too).
 *
 * `null` means "no mark applies" — the caller draws the generic icon.
 */
export function markFor(baseUrl: string, type: GatewayProviderType): MarkKey | null {
  const host = hostOf(baseUrl);
  if (host === null) return typeMark(type);
  if (isSelfHosted(host)) return "selfhosted";
  // First match wins; no two entries in the table can match one host today,
  // and a new entry that overlaps an existing one has to be added above it.
  for (const domain of DOMAIN_KEYS) {
    if (host === domain || host.endsWith(`.${domain}`)) return DOMAIN_MARKS[domain];
  }
  return typeMark(type);
}

/* ----------------------------------------------------------------------------
   Type-level pins (see lib/typeAssert.ts). There is no TypeScript test harness
   here, so `tsc --noEmit` is the whole frontend gate and a value that must not
   drift is pinned by giving it a literal type and asserting it exactly.

   What these cover is the *mapping*: that the table stays keyed on registrable
   domains rather than hosts (which is the subdomain-collapse rule's table
   half), that GLM's type fallback is Z.AI's mark and not its own, and that
   `markFor` still answers `null` for the no-mark case rather than a default.
   What they cannot cover is `new URL()` at runtime, so that half is verified by
   exercising `markFor` directly — see the PR for the cases and their answers.
   ------------------------------------------------------------------------- */

/** A host, not a registrable domain, must never be a key — `api.openai.com`
 *  reaches `openai.com` through the label-boundary match instead. */
export type PinDomainsAreRegistrable = Expect<
  Eq<"api.openai.com" extends keyof typeof DOMAIN_MARKS ? true : false, false>
>;

/** The registrable domain both `https://api.openai.com/v1` and
 *  `https://openai.com/v1` collapse onto. */
export type PinOpenAiDomain = Expect<Eq<(typeof DOMAIN_MARKS)["openai.com"], "openai">>;

/** An empty `base_url` keys on the type, and GLM's upstream is Z.AI. */
export type PinGlmTypeFallback = Expect<Eq<(typeof TYPE_MARKS)["glm"], "zai">>;

/** The unknown case is `null` — not a silently-chosen default mark. */
export type PinNoMarkIsNull = Expect<Eq<ReturnType<typeof markFor>, MarkKey | null>>;

/**
 * The provider's tile: its brand mark, or the generic icon when nothing in the
 * tables applies.
 */
export function BrandMark({
  provider,
}: {
  provider: Pick<GatewayProviderSummary, "base_url" | "type">;
}) {
  const key = markFor(provider.base_url, provider.type);
  if (key === null) {
    return (
      <div className="avatar avatar--accent">
        <Icon name="database" size={14} />
      </div>
    );
  }
  const mark = MARKS[key];
  return (
    <div className={`avatar avatar--${mark.tone}`} title={mark.title}>
      {mark.icon ? <Icon name={mark.icon} size={14} /> : mark.label}
    </div>
  );
}
