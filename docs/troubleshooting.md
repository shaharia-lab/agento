# Troubleshooting

Common problems and what to do about them.

- [Installing and launching](#installing-and-launching)
- [The Claude Code CLI](#the-claude-code-cli)
- [Chats](#chats)
- [History and analytics](#history-and-analytics)
- [Scheduled tasks](#scheduled-tasks)
- [Integrations](#integrations)
- [LLM Gateway](#llm-gateway)
- [Updates](#updates)
- [Reading the logs](#reading-the-logs)
- [Still stuck](#still-stuck)

---

## Installing and launching

### macOS says the app is damaged or cannot be verified

The app is not signed with a paid Apple Developer certificate, so macOS blocks
the first launch. Open **System Settings → Privacy & Security**, find the line
about Agento, and click **Open Anyway**.

If that line is not there:

```bash
xattr -dr com.apple.quarantine /Applications/Agento.app
```

Only the first launch needs this.

### Windows SmartScreen blocks the installer

Click **More info**, then **Run anyway**. The installer is not code signed, so
SmartScreen has no publisher to recognize.

### The AppImage will not start

Most often a missing FUSE library. Either install it:

```bash
sudo apt install libfuse2      # Debian, Ubuntu
```

or run without it:

```bash
./Agento_1.0.0_amd64.AppImage --appimage-extract-and-run
```

Also check the file is executable: `chmod +x Agento_*.AppImage`.

### The .deb or .rpm will not install

Your package manager could not find GTK 3 or WebKitGTK 4.1. That usually means
the distribution release is older than WebKitGTK 4.1. Use the AppImage instead.

### The window opens blank or white

Your system's webview is too old or missing.

- **Linux**: install `libwebkit2gtk-4.1-0`.
- **Windows**: install the
  [WebView2 runtime](https://developer.microsoft.com/microsoft-edge/webview2/).
  The installer normally handles this.

Restart the app after installing.

### Launching Agento does nothing

It is probably already running. Agento allows one copy at a time, and a second
launch focuses the existing window instead of opening a new one. Check your dock,
taskbar or window list.

---

## The Claude Code CLI

### "Claude Code CLI not found"

Agents run by launching `claude`. Agento looks for it on your `PATH` and in the
usual per-user install locations (`~/.local/bin`, `~/.npm-global/bin`,
`~/.bun/bin`, `~/.volta/bin`, `~/bin`, and `AppData/Roaming/npm` on Windows).

First check it works in a terminal:

```bash
claude --version
```

If that works but Agento still cannot find it, the cause is almost always that a
GUI app inherits a smaller `PATH` than your shell does. Point Agento at the
binary directly:

```bash
AGENTO_CLAUDE_EXECUTABLE=/full/path/to/claude
```

Set that in your environment before launching the app. `which claude` gives you
the path.

### Chats fail with an authentication error

Agento does not manage Claude authentication. Sign in with the CLI:

```bash
claude
```

Complete the sign-in there, then retry in Agento.

---

## Chats

### A chat is stuck mid-stream

Click **Stop**. If the composer is still blocked afterwards, reload the
transcript from the toolbar.

If it keeps happening, check the log for the run. A `claude` subprocess that
crashed ends the stream with nothing to say why, and the log is where the reason
lands.

### An agent will not use a tool

Tools are an allowlist. Open the agent and confirm the tool is ticked. For
integration tools, also confirm that integration's service is enabled and that
its credentials are still valid.

### The agent cannot see my files

Check the chat's working directory. An agent only reaches the folder the chat was
started in.

### The agent keeps asking permission

Asking is the default in a chat, because you are there to answer. Each chat
carries its own permission mode, so to stop the prompts for a conversation you
trust, start it with **Permissions → Never ask**.

The setting is chosen when the chat is created and the inspector shows what a
chat is running under. Existing chats keep whatever they were created with;
chats created before this setting existed fall back to the agent's mode, and to
asking if the agent has no preference.

Unattended runs, meaning scheduled tasks, never prompt, because nothing could
answer.

If a tool is denied without any prompt at all, it is not on the agent's tool
list. Add it there — the allowlist is enforced whatever the permission mode
says.

---

## History and analytics

### The sessions list is empty

If it says "Scanning" with a count, the first index is still running. Wait for it.

If it says there are no sessions, Agento found no Claude Code history where it
looked. Check **Settings → Claude → Indexed directories** covers the directory
your transcripts are in. The default is `~/.claude`.

### Sessions from a second Claude account are missing

Add that account's configuration directory in
**Settings → Claude → Indexed directories**. Both accounts then appear in every
total.

### A project is missing from every chart

Check **Settings → Data → Hidden projects**. Unhiding is immediate and costs
nothing.

### Costs look wrong

Cost is computed from the price catalog in **Settings → Pricing**, at the price
in effect when each message was sent.

- A model with no entry contributes no cost, and the totals say how many tokens
  were unpriced.
- If a price in the catalog is wrong, use **Correct a rate**, not "add a rate".
  Correcting rewrites history; adding only affects messages after that date.

Either way, sessions re-price in the background afterwards. It can take a few
minutes on a large history.

### Durations look too short

They are meant to. Duration means active time, not the span from first to last
message. A session resumed a week later would otherwise report a week. Adjust the
threshold in **Settings → Data → Idle gap threshold**.

### A scan seems to run for no reason

Some changes invalidate every stored figure and force a full re-read: a price
edit, and a change to the idle gap threshold. That is expected, it runs in the
background, and the app stays usable.

---

## Scheduled tasks

### A task never runs

Check, in order:

1. The task is **Enabled**.
2. The inspector shows a **Next run** in the future.
3. **Stop after** has not been reached and **Stop at** has not passed.
4. Agento is actually running. A desktop app that is closed fires nothing.

### Every task fires twice

Two Agento processes are sharing one data directory. Agento normally prevents a
second copy from starting, so this means one of them was pointed at the same
directory deliberately with `AGENTO_DATA_DIR`. Close one, or give it its own
directory.

### A run failed with a timeout

The run took longer than the task's **Timeout**. Raise it, or make the prompt
narrower.

---

## Integrations

### Credentials stopped working after an edit

Re-enter them. Agento asks for credentials again when you edit an integration
that has stored ones, because saving the form without them would wipe the working
credential.

### An OAuth window did not come back

Close the browser window and try **Connect** again. If your browser blocked the
redirect to a local address, allow it and retry.

### Tools from an integration are not offered to an agent

Three things have to line up: the integration is connected, the service is
enabled inside it, and the tool is ticked on the agent.

### WhatsApp is listed but unusable

Agento does not support WhatsApp. An integration created by an older version is
still listed and its data is safe, but it cannot be edited or used.

---

## LLM Gateway

### Overview says "Port unavailable"

Something else already holds that port. Change it in **LLM Gateway → Gateway
Settings → Port** and save; the listener rebinds immediately.

The usual culprit is a second Agento. A development build and an installed one
read different databases but share the machine's ports, so both can be configured
for 8880 and only the first to start gets it. The status carries the exact reason,
and the same line is in the log:

```
binding the llm gateway on 127.0.0.1:8880: Address already in use (os error 98)
```

### The gateway answers 401

The token is absent, malformed, expired, revoked, or was signed by a key this
install no longer uses — regenerating the signing key does that to every token at
once. Mint a new one from **LLM Gateway → Overview** and paste it into the tool
again.

Check the tool is actually sending it: the gateway accepts either
`Authorization: Bearer <token>` or `x-api-key: <token>`, and an `Authorization`
header that is present but not a `Bearer` is ignored in favour of `x-api-key`.

### The gateway answers 403

You used a `read` or `write` token. Those reach the Agento API and are refused
here on purpose. Mint one with the **`llm`** scope — Overview's **Create gateway
token** button does exactly that — and use it instead.

A bigger Agento token is not the fix. The scopes are disjoint rather than ranked,
so `write` does not include `llm`.

### The Agento API answers 403 for a token that works on the gateway

The same rule the other way round. An `llm` token reaches the gateway and nothing
under `/api`; there is no Agento API route that accepts one. Use a `read` or
`write` token for the API.

### The tool connects but every request fails with a model error

The name your tool sends as `model` has to be an alias you defined, exactly.
There is no prefix parsing and no fuzzy matching, so an unconfigured name is a
404:

```
model alias 'claude-opus-5' is not configured on this gateway
```

For Claude Code this is the default first experience, because it asks for its own
default model unless told otherwise and stops with *"There's an issue with the
selected model"*. Either `export ANTHROPIC_MODEL=<your alias>`, or name an alias
after the model Claude Code asks for.

### Claude Code cannot reach it at all

Check the base URL has no `/v1`. Claude Code and the Anthropic SDK append
`/v1/messages` themselves, so `ANTHROPIC_BASE_URL` ends at `/anthropic`:

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:8880/anthropic   # right
export ANTHROPIC_BASE_URL=http://127.0.0.1:8880/anthropic/v1 # 404 on every call
```

The OpenAI-shaped base URL is the opposite and does end in `/v1`.

### Nothing appears in Usage

A row is written per request the gateway *served*, so a request refused before it
got that far — a bad token, a 404 on the base URL — records nothing on purpose.
Confirm the listener is up and the request is arriving:

```bash
curl http://127.0.0.1:8880/healthz
curl http://127.0.0.1:8880/v1/models -H "Authorization: Bearer <your gateway token>"
```

`/healthz` needs no credential. `/v1/models` lists your aliases; if the alias you
expect is missing or disabled, that is why traffic is not reaching it.

### Usage shows requests but no cost

Cost is computed from the price catalog, which ships filled in for Anthropic,
Moonshot, Z.ai and Alibaba models — so OpenAI and Gemini traffic has no rate.
Anything unpriced is recorded as such rather than as free, which is why the total
is labelled a floor. Add rates under **Settings → Pricing** for the models you
actually use.

### The gateway's log lines

The same `Agento.log` as everything else — see [Reading the
logs](#reading-the-logs). Gateway lines are prefixed `llm gateway` or `gateway`:

```
llm gateway listening on http://127.0.0.1:8880
gateway completion streaming alias=fast provider=my-openai model_id=gpt-4o-mini
gateway usage pruned rows=1204 older_than_days=90
```

---

## Updates

### The app says an update is available but there is no install button

You installed from a `.deb` or `.rpm`. Those are notify only, because your package
manager owns the installed files. Download the new package and install it the way
you installed the first one, or switch to the AppImage for in-app updates.

### Can I go back to an older version?

Not below 0.1.1. That release added a database column, and Agento refuses to
write to a database newer than itself rather than corrupting it — so an older
build would appear to fail on every action.

Your data is not damaged by this. If you need an older build, restore the
`~/.agento` backup you took before upgrading.

### The update download fails

Check your network and try again from **About → Check for updates**. Updates are
downloaded from GitHub, so a proxy or a firewall that blocks it will stop them.

If it keeps failing, download the release manually and install over the top. Your
data is untouched by a reinstall.

### I do not want update checks

**Settings → General → Updates → Never check**.

---

## Reading the logs

| Platform | Path |
| --- | --- |
| Linux | `~/.local/share/com.shaharialab.agento/logs/Agento.log` |
| macOS | `~/Library/Logs/com.shaharialab.agento/Agento.log` |
| Windows | `%LOCALAPPDATA%\com.shaharialab.agento\logs\Agento.log` |

The live file plus three dated archives are kept, roughly 20 MB in total.

The log records one line per API request, plus what each write did. It does
**not** record message bodies, prompts, credentials or search terms. It does
record agent slugs and file paths, so treat it as mildly sensitive when sharing.

---

## Still stuck

Open an issue at
[github.com/shaharia-lab/agento/issues](https://github.com/shaharia-lab/agento/issues)
with:

- Your platform and how you installed (dmg, exe, AppImage, deb, rpm).
- The Agento version from **About**.
- `claude --version`.
- What you did and what happened.
- The relevant lines from the log.
