/* ============================================================================
   API client for the Agento backend.

   Requests go to a relative /api path so the page is always same-origin with
   the server: in development Vite proxies /api to the Rust proxy, and in
   release the Rust proxy serves this page itself. That is what keeps the
   browser out of CORS and leaves SSE working.

   Every request also carries a bearer token (#400, #405) — see `authHeaders`.
   There are exactly two places headers are built, and both are below; a third
   would be a request that 401s.

   Since #405 the token is a JWT signed by the install's Ed25519 keypair rather
   than an opaque per-launch string, and it can stop being valid while the page
   is still open: its `exp` can pass, or the user can regenerate the keypair from
   Settings → Security, which is *supposed* to sign this window out along with
   everything else. `withAuth` below is the recovery — one re-mint, one retry,
   never a loop.
   ========================================================================== */

import { hostInfo, resetHostInfo } from "./tauri";

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
    public body?: unknown
  ) {
    super(message);
    this.name = "ApiError";
  }
}

const BASE = "/api";

/**
 * What a 401 means when this page never had a token to send (#400).
 *
 * The server cannot tell our own SPA loaded in Chrome from an attacker's page,
 * and must not try — so the explanation is produced here, where "did we have a
 * token" is actually known.
 */
const NO_TOKEN_HINT =
  "Agento's API is only reachable from the desktop app window. " +
  "Opening this address in a browser will not work.";

/**
 * The headers every request carries.
 *
 * The bearer token (#400) is minted per launch by the Rust side and delivered
 * over Tauri IPC, so this awaits `hostInfo()` — memoized, so it is one IPC round
 * trip per page, not one per request. **Awaiting it inside the request is the
 * point**: a token resolved into a module-level variable at import time would
 * race every view that fetches from its first effect, and the symptom would be
 * an occasional 401 on cold start that looks like a server bug.
 *
 * A missing token is **not** an error here. Outside Tauri there is none to have,
 * and in `npm run dev` Chrome talks to the app through Vite's proxy, which adds
 * the header server-side from the dev token file — so refusing to send the
 * request would break the one workflow the dev token file exists for. Let the
 * server decide; a genuine 401 is then explained below.
 */
async function authHeaders(accept: string): Promise<Record<string, string>> {
  const headers: Record<string, string> = {
    Accept: accept,
    // The server requires this on every request, not just those with a body —
    // its requireJSONContentType guard runs before the handler.
    "Content-Type": "application/json",
  };
  const info = await hostInfo();
  if (info?.api_token) headers.Authorization = `Bearer ${info.api_token}`;
  return headers;
}

/** A 401 we can explain, or the server's own message. */
async function rejection(
  res: Response,
  headers: Record<string, string>
): Promise<ApiError> {
  if (res.status === 401 && !headers.Authorization) {
    return new ApiError(res.status, NO_TOKEN_HINT);
  }
  return new ApiError(res.status, await errorMessage(res), await safeJson(res));
}

/**
 * Send a request, re-minting the credential and retrying **exactly once** on a
 * 401 (#405).
 *
 * A token can now go stale under an open page — `exp` passes, or the user
 * regenerates the keypair — and `host_info` mints a fresh one on every
 * invocation, so dropping the memo and asking again is a complete recovery. The
 * alternative is a window that has to be restarted after a regenerate, which
 * would make "revoke everything" a thing nobody dares press.
 *
 * **The bound is structural, not a counter**, and that is deliberate: a retry
 * loop against a server that is genuinely 401ing — a revoked token, a clock
 * skewed past the leeway, a `read` token used by mistake — is worse than the bug
 * it was meant to fix, because it turns one failed request into a spin. `send`
 * takes the `fetch` to run and calls it at most twice, and there is no path by
 * which the second call can retry again; expressing a third attempt would mean
 * changing this function's shape rather than a constant. (The repo has no
 * frontend test runner, so this is covered by construction and by driving a live
 * regenerate in the real webview, which is what `ui-verify` is for.)
 *
 * A 401 with **no** `Authorization` header attached is not retried: the page
 * never had a token, so re-asking cannot produce one, and `rejection` turns it
 * into the one honest message instead.
 *
 * **Retrying is safe on every method, `POST` and `DELETE` included, and the
 * reason is a property of the server rather than of this function.** A 401 is
 * answered by `guards.rs::reject`, which runs *before* routing — so the handler
 * never ran, no row was written, no subprocess was spawned, and there is nothing
 * for the second attempt to duplicate. That is the whole argument, and it is
 * written here rather than at a call site because it is what makes a blanket
 * retry acceptable at all. If a 401 ever became something a handler could answer
 * *after* doing work, this would have to become method-aware.
 */
async function withAuth<T>(
  accept: string,
  send: (headers: Record<string, string>) => Promise<Response>,
  read: (res: Response, headers: Record<string, string>) => Promise<T>
): Promise<T> {
  let headers = await authHeaders(accept);
  let res = await send(headers);

  if (res.status === 401 && headers.Authorization) {
    resetHostInfo();
    headers = await authHeaders(accept);
    // The second and last attempt. Whatever this answers is the answer.
    res = await send(headers);
  }

  return read(res, headers);
}

async function request<T>(
  method: string,
  path: string,
  body?: unknown,
  signal?: AbortSignal
): Promise<T> {
  return withAuth(
    "application/json",
    (headers) =>
      fetch(BASE + path, {
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
        signal,
      }),
    async (res, headers) => {
      if (!res.ok) {
        throw await rejection(res, headers);
      }

      if (res.status === 204) return undefined as T;

      const text = await res.text();
      if (!text) return undefined as T;
      return JSON.parse(text) as T;
    }
  );
}

async function errorMessage(res: Response): Promise<string> {
  try {
    const clone = res.clone();
    const data = await clone.json();
    if (data && typeof data === "object" && "error" in data) {
      return String((data as { error: unknown }).error);
    }
  } catch {
    /* fall through to the status line */
  }
  return `${res.status} ${res.statusText}`;
}

async function safeJson(res: Response): Promise<unknown> {
  try {
    return await res.clone().json();
  } catch {
    return undefined;
  }
}

export const api = {
  get: <T>(path: string, signal?: AbortSignal) =>
    request<T>("GET", path, undefined, signal),
  post: <T>(path: string, body?: unknown, signal?: AbortSignal) =>
    request<T>("POST", path, body, signal),
  put: <T>(path: string, body?: unknown, signal?: AbortSignal) =>
    request<T>("PUT", path, body, signal),
  patch: <T>(path: string, body?: unknown, signal?: AbortSignal) =>
    request<T>("PATCH", path, body, signal),
  del: <T>(path: string, body?: unknown, signal?: AbortSignal) =>
    request<T>("DELETE", path, body, signal),
};

/** Build a query string, dropping empty values so the server sees no param. */
export function qs(params: Record<string, string | number | boolean | undefined | null>): string {
  const sp = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v === undefined || v === null || v === "") continue;
    sp.set(k, String(v));
  }
  const s = sp.toString();
  return s ? `?${s}` : "";
}

/* ============================================================================
   SSE — chat streaming.

   The stream is the response to a POST, so EventSource (GET-only) cannot be
   used. This reads the body directly and parses the event framing.
   ========================================================================== */

export interface SSEMessage {
  event: string;
  data: string;
}

export interface StreamHandlers {
  onEvent(msg: SSEMessage): void;
  onError?(err: Error): void;
  onDone?(): void;
}

/**
 * POST a message to a chat and consume the SSE response.
 * Returns a function that aborts the stream.
 */
export function streamChatMessage(
  chatId: string,
  content: string,
  handlers: StreamHandlers
): () => void {
  const controller = new AbortController();

  (async () => {
    try {
      // The stream is a POST response, so this is `fetch` rather than
      // `EventSource` — which is GET-only and cannot set headers. That is also
      // what lets the turn carry the bearer token like any other request (#400);
      // an EventSource-based stream would have needed a different scheme.
      //
      // It goes through `withAuth` for the same reason every other request does
      // (#405): a turn started just after a regenerate would otherwise be the
      // one request that cannot recover, and it is the longest and most
      // expensive one to lose. See `withAuth` for why retrying a `POST` is safe.
      const body = await withAuth(
        "text/event-stream",
        (headers) =>
          fetch(`${BASE}/chats/${chatId}/messages`, {
            method: "POST",
            headers,
            body: JSON.stringify({ content }),
            signal: controller.signal,
          }),
        async (res, headers) => {
          if (!res.ok) {
            throw await rejection(res, headers);
          }
          if (!res.body) throw new Error("response has no body");
          return res.body;
        }
      );

      await consumeSSE(body, handlers, controller.signal);
      handlers.onDone?.();
    } catch (err) {
      if (controller.signal.aborted) {
        handlers.onDone?.();
        return;
      }
      handlers.onError?.(err instanceof Error ? err : new Error(String(err)));
    }
  })();

  return () => controller.abort();
}

/**
 * Parse an SSE byte stream into events.
 *
 * Frames are separated by a blank line and can be split across chunks at any
 * byte, so the tail of each read is carried forward rather than parsed.
 */
async function consumeSSE(
  body: ReadableStream<Uint8Array>,
  handlers: StreamHandlers,
  signal: AbortSignal
): Promise<void> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";

  try {
    while (!signal.aborted) {
      const { done, value } = await reader.read();
      if (done) break;

      buffer += decoder.decode(value, { stream: true });

      let sep: number;
      while ((sep = indexOfFrameEnd(buffer)) !== -1) {
        const raw = buffer.slice(0, sep);
        buffer = buffer.slice(sep).replace(/^(\r?\n){2}/, "");
        const msg = parseFrame(raw);
        if (msg) handlers.onEvent(msg);
      }
    }
  } finally {
    reader.releaseLock();
  }
}

function indexOfFrameEnd(buf: string): number {
  const lf = buf.indexOf("\n\n");
  const crlf = buf.indexOf("\r\n\r\n");
  if (lf === -1) return crlf;
  if (crlf === -1) return lf;
  return Math.min(lf, crlf);
}

function parseFrame(raw: string): SSEMessage | null {
  let event = "message";
  const dataLines: string[] = [];

  for (const line of raw.split(/\r?\n/)) {
    if (!line || line.startsWith(":")) continue;
    const colon = line.indexOf(":");
    const field = colon === -1 ? line : line.slice(0, colon);
    // A single leading space after the colon is part of the framing, not data.
    let value = colon === -1 ? "" : line.slice(colon + 1);
    if (value.startsWith(" ")) value = value.slice(1);

    if (field === "event") event = value;
    else if (field === "data") dataLines.push(value);
  }

  if (!dataLines.length) return null;
  return { event, data: dataLines.join("\n") };
}
