# Security and network exposure

Agento is a single-user desktop application. It ships **without
authentication**, on purpose — there are no accounts to manage on a tool that
runs on the machine you are already logged into.

That choice is only safe while nothing else can reach the API. This page
describes what protects it, what does not, and what you have to do yourself if
you expose it.

- [What the API can do](#what-the-api-can-do)
- [The default: loopback only](#the-default-loopback-only)
- [Browser-based protections](#browser-based-protections)
- [Reaching Agento from another device](#reaching-agento-from-another-device)
- [Behind a reverse proxy or tunnel](#behind-a-reverse-proxy-or-tunnel)
- [Inbound webhooks](#inbound-webhooks)
- [Where your data lives](#where-your-data-lives)
- [Agent permission modes](#agent-permission-modes)
- [Reporting a vulnerability](#reporting-a-vulnerability)

---

## What the API can do

Treat access to the Agento API as **shell access to the machine**. Two requests
are enough:

```
POST /api/agents            → create an agent with the Bash tool and bypass permissions
POST /api/chats/{id}/messages → run it
```

There is no configuration that makes this untrue. Every protection below exists
to control *who can send those requests*.

---

## The default: loopback only

`AGENTO_BIND` defaults to `127.0.0.1`, so out of the box only processes on your
own machine can connect. The startup log names the interface it bound to.

> **Upgrading from an older version?** Agento used to listen on every interface.
> If you reached it from another device and that stopped working, see
> [below](#reaching-agento-from-another-device).

---

## Browser-based protections

A loopback bind does not help against a web page you visit, because the browser
is already inside the loopback boundary. Two middlewares, scoped to `/api`,
close that route.

**1. State-changing requests must declare JSON** — otherwise `415 Unsupported
Media Type`.

A cross-origin `POST` carrying `Content-Type: text/plain` is a CORS *simple
request*: the browser sends it with **no preflight**. The attacker cannot read
the response, but the side effect has already happened — and handlers decode
JSON regardless of the declared type. Requiring `application/json` removes the
simple-request status, which forces a preflight that same-origin CORS then
refuses.

`GET`, `HEAD` and `OPTIONS` are untouched. A body-less `POST` is deliberately
*not* exempt, because a body-less cross-origin `POST` is itself a simple
request. `multipart/form-data` is admitted for the one endpoint that uploads
files.

**2. The `Host` header must name a host Agento is served under** — otherwise
`403 Forbidden`.

DNS rebinding defeats CORS completely: an attacker's domain that resolves to
`127.0.0.1` is *same-origin* as far as the browser is concerned, so no
cross-origin rule applies at all. Validating `Host` is what stops it.

Accepted: `localhost`, any loopback IP, the host of your configured
[public URL](#behind-a-reverse-proxy-or-tunnel), and the bind address — where a
bind of `0.0.0.0` or `::` admits any bare **IP literal**, since clients dial the
machine's LAN address rather than the wildcard. That keeps the rebinding
property intact, because rebinding needs a *name* whose DNS the attacker
controls, and a name is never an IP literal. Reaching Agento over the LAN under
a hostname needs the public URL set.

**Both guards apply to `/api` only.** `/health`, `/metrics` and
`POST /webhooks/telegram/{id}` are outside them — the webhook arrives from
Telegram's servers with a foreign `Host` and is authenticated by
[its own secret](#inbound-webhooks) instead.

---

## Reaching Agento from another device

```bash
AGENTO_BIND=0.0.0.0 agento web
```

This exposes an unauthenticated API that can run commands on your machine to
**everyone on that network**. Only do it on a network you trust, or put a proxy
that authenticates in front of it.

---

## Behind a reverse proxy or tunnel

If you reach Agento under a hostname rather than an IP, tell it that hostname or
every API request will be refused with `403`:

- **Settings → General → Public URL**, or
- the `AGENTO_PUBLIC_URL` environment variable, which locks the field in the UI.

```bash
AGENTO_PUBLIC_URL=https://agento.example.com AGENTO_BIND=0.0.0.0 agento web
```

The stored value is re-read per request, so setting it in the UI takes effect
immediately — no restart. A value without a scheme (`agento.example.com`) is
accepted.

Agento does not terminate TLS. If it is reachable beyond your machine, put HTTPS
and authentication on the proxy.

---

## Inbound webhooks

Telegram triggers require a publicly reachable URL, which is what the public URL
setting is for. Registering the webhook generates a secret token that Telegram
sends back in the `X-Telegram-Bot-Api-Secret-Token` header; Agento verifies it on
every delivery and rejects anything else. Rotate it from the integration page if
it may have leaked.

---

## Where your data lives

Everything is local, in `~/.agento` (or `AGENTO_DATA_DIR`):

| Path | Contents |
|------|----------|
| `agento.db` | Agents, chats, tasks, integrations, settings, cached Claude session analytics |
| `logs/system.log` | Rotating application log |
| `logs/sessions/<id>.log` | Per-session logs |
| `mcps.yaml` | External MCP server registry |

**Integration credentials — bot tokens, API tokens, OAuth refresh tokens — are
stored in that database as-is.** There is no encryption at rest, and the
database is only as protected as the file permissions on your home directory.
Treat `~/.agento/agento.db` as a secret, and do not sync it to a shared
location.

Claude Code's own transcripts are read from `~/.claude` (and any other
[indexed config directory](claude-sessions.md#multiple-claude-accounts)) and
never modified. Nothing is uploaded anywhere: there is no account, no telemetry
and no server component. OpenTelemetry export is off unless you configure it
yourself — see [Monitoring](monitoring.md).

---

## Agent permission modes

An agent's permission mode decides what happens when it wants to use a tool:

| Mode | Behaviour |
|------|-----------|
| `bypass` (default) | Tools run without asking |
| `default` | Claude Code's normal prompting |
| `plan` | Plans first, acts only after you approve |
| `dontAsk` | Runs without prompting for permitted tools |

`bypass` is the default because agents are usually run unattended, but it means
an agent holding `Bash` can do anything you can. Give an agent only the tools it
needs, and prefer `plan` for anything that writes files or runs commands —
especially for scheduled tasks and Telegram triggers, where nobody is watching
the run.

---

## Reporting a vulnerability

See [SECURITY.md](../SECURITY.md).
