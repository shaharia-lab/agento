/* ============================================================================
   The env snippets the Overview hands out (#427).

   Everything a user copies out of this app to point a tool at the gateway is
   built here, in one file, because the single most likely user-facing bug in
   this feature is one character of base URL:

       OpenAI      http://127.0.0.1:<port>/v1
       Anthropic   http://127.0.0.1:<port>/anthropic      ← no /v1

   The asymmetry is real and is not a typo. The OpenAI SDKs take a base URL that
   already includes the version segment and append `/chat/completions` to it,
   while the Anthropic SDK and Claude Code append `/v1/messages` themselves — so
   an `/anthropic/v1` here would reach `/anthropic/v1/v1/messages`, which the
   gateway does not route.

   The frontend has no unit-test harness, so both strings are pinned **at the
   type level** instead: the two functions declare template-literal return types
   and the `Pin*` aliases below assert what they produce for a fixed port. That
   makes `npm run build` — the repo's actual gate — fail on a changed literal,
   which is the property a test would have bought.
   ========================================================================== */

import type { GatewayStatus } from "../../lib/types";

/** The gateway binds `127.0.0.1` unconditionally; there is no public URL. */
const HOST = "http://127.0.0.1";

/** The placeholder a snippet shows before a token has been minted. */
export const TOKEN_PLACEHOLDER = "<your gateway token>";

export function openaiBaseUrl<P extends number>(
  port: P
): `http://127.0.0.1:${P}/v1` {
  return `${HOST}:${port}/v1`;
}

export function anthropicBaseUrl<P extends number>(
  port: P
): `http://127.0.0.1:${P}/anthropic` {
  return `${HOST}:${port}/anthropic`;
}

export function healthUrl<P extends number>(
  port: P
): `http://127.0.0.1:${P}/healthz` {
  return `${HOST}:${port}/healthz`;
}

/* --- The compile-time pin ------------------------------------------------- */

/** Exact type equality — `extends` alone would accept a wider literal. */
type Eq<A, B> =
  (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2
    ? true
    : false;

type Expect<T extends true> = T;

/**
 * Change either literal above and these stop compiling. Exported so
 * `noUnusedLocals` does not simply delete the guard by complaining about it.
 */
export type PinOpenAIBaseUrl = Expect<
  Eq<ReturnType<typeof openaiBaseUrl<4141>>, "http://127.0.0.1:4141/v1">
>;
export type PinAnthropicBaseUrl = Expect<
  Eq<
    ReturnType<typeof anthropicBaseUrl<4141>>,
    "http://127.0.0.1:4141/anthropic"
  >
>;
export type PinHealthUrl = Expect<
  Eq<ReturnType<typeof healthUrl<4141>>, "http://127.0.0.1:4141/healthz">
>;

/* --- The snippets --------------------------------------------------------- */

export interface Snippet {
  key: string;
  title: string;
  /** Why a user would pick this one, in one line. */
  note: string;
  body: string;
}

/**
 * Build every snippet for a port and a token.
 *
 * `token` is the freshly minted credential when one exists in this render and
 * [`TOKEN_PLACEHOLDER`] otherwise — there is no third state, because nothing
 * stores a token and no response can return one.
 */
export function snippetsFor(port: number, token?: string): Snippet[] {
  const secret = token ?? TOKEN_PLACEHOLDER;
  const openai = openaiBaseUrl(port);
  const anthropic = anthropicBaseUrl(port);

  return [
    {
      key: "openai",
      title: "OpenAI SDK",
      note: "Any client that reads OPENAI_BASE_URL — the OpenAI SDKs, LiteLLM, Aider.",
      body: [`export OPENAI_BASE_URL=${openai}`, `export OPENAI_API_KEY=${secret}`].join(
        "\n"
      ),
    },
    {
      key: "anthropic",
      title: "Anthropic SDK",
      note: "The base URL has no /v1 — the SDK appends it.",
      body: [
        `export ANTHROPIC_BASE_URL=${anthropic}`,
        `export ANTHROPIC_AUTH_TOKEN=${secret}`,
      ].join("\n"),
    },
    {
      key: "claude-code",
      title: "Claude Code",
      note: "Same two variables, exported before you launch the CLI.",
      body: [
        `export ANTHROPIC_BASE_URL=${anthropic}`,
        `export ANTHROPIC_AUTH_TOKEN=${secret}`,
        "claude",
      ].join("\n"),
    },
    {
      key: "curl",
      title: "Check it with curl",
      // The app cannot make this request itself: the webview's own session is
      // a `write` token and the gateway only accepts `llm`, so an in-app "test
      // connection" button could only ever answer 403. A copyable command is
      // the honest version of that feature.
      note: "Agento cannot run this for you — its own credential is scoped to /api, not to the gateway.",
      body: [
        `curl ${openai}/models \\`,
        `  -H "Authorization: Bearer ${secret}"`,
      ].join("\n"),
    },
  ];
}

/** The port a snippet should use: the live one when known, else the setting. */
export function effectivePort(
  status: GatewayStatus | undefined,
  configured: number
): number {
  return status?.port ?? configured;
}
