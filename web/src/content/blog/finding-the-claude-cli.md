---
title: Finding the Claude CLI where it actually is
description: A user ran claude --version, got 2.1.231, and Agento told them Claude Code was not installed. They were both right.
date: 2026-08-25
tags: [Engineering, Release 1.2.0]
featured: true
---

Agento does not ship the Claude Code CLI. It spawns it — every agent run is a
subprocess, and the CLI's own sign-in is the credential. So the first thing the
app does on launch is work out where `claude` lives, and the first thing it did
wrong was assume the answer was on `PATH`.

## A GUI app has a different PATH than your terminal

This is the part that surprises people, and it surprised us. When you launch an
application from Finder, the Dock or Spotlight, it inherits launchd's
environment rather than your shell's. That is
`/usr/bin:/bin:/usr/sbin:/sbin` and nothing else. Every line your `.zshrc`
exported is invisible.

> The launch that matters — from the Dock, by someone who has not opened a
> terminal that day — is exactly the launch where scanning `PATH` finds
> nothing.

Worse, there is an install shape that is on no `PATH` at all.
`claude migrate-installer` puts the binary in `~/.claude/local` and wires it up
as a shell *alias*. No amount of scanning directories finds it, because there is
no file to find under any name you would look for.

## Five rules, in order

Resolution is now a single function with an explicit order, cached once per
launch:

```text
1. AGENTO_CLAUDE_EXECUTABLE        explicit override, trusted
2. claude_executable_path setting  stored, trusted
3. $SHELL -lic 'command -v claude' login shell — finds aliases
4. the process PATH                works when launched from a terminal
5. known install locations         last resort, verified
```

Rules three through five verify what they find: the path must be an executable
file **and** answer `--version` with Claude Code's banner, so an unrelated
`claude` on the `PATH` is skipped rather than spawned on every turn. Rules one
and two are taken on trust — a wrapper script is a documented reason to set
them, and refusing one would remove the escape hatch the setting exists to be.

## The banner and the spawn had to become one answer

The reported bug was really a pair. The startup banner said the CLI was
missing, *and* agents genuinely could not run — because the fallback was the
bare name `claude`, which fails to resolve for exactly the same reason the
banner did. Two lookups that agreed on being wrong.

They are one function now. If the banner is wrong, every run is wrong, and you
find out at launch instead of at the first message.

## The bounds are a startup budget, not a ceiling

Both subprocesses run inside the app's setup block, before the window shows.
That makes three seconds for the shell probe and two per `--version` the worst
case a user waits for a window — not a generous limit. A pathological shell
degrades to "found by a later rule", never to a window that does not open.

Resolution happens once per launch, so saving the setting takes effect at the
next start. Re-detecting live was considered and left out: a `OnceLock` filled
by a background thread races the first caller, and the loser fills it *without*
the stored override — which would mean a configured path being ignored on some
launches and not others.
