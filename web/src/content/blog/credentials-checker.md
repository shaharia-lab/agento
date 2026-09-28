---
title: "Credentials Checker: find the API keys you leaked to Claude Code"
description: Every Claude Code session is saved on your disk, including any secret that passed through it. Agento 1.4.0 scans those transcripts and tells you which keys to rotate.
date: 2026-09-28
tags: [Security, Release 1.4.0]
featured: true
image: /blog/credentials-checker.png
imageAlt: A chat transcript with one line highlighted in red, a magnifying glass showing it masked, and a green shield with a check mark
---

You paste an error log into Claude Code to get help. The log contains a
database URL with the password in it. Or the agent runs `cat .env` to debug a
config problem, and your Stripe key scrolls past. You fix the bug, close the
terminal and forget about it.

The secret did not go away. It went into the model's context, and it is still
sitting in a transcript file on your disk.

## Where your secrets end up

Claude Code saves every session as a plain text file under
`~/.claude/projects`. That is what lets you resume a conversation later, and it
is also why a key that was pasted once, printed once, or read from a file once
is now stored in full, forever, in a place nobody thinks to look.

That creates two problems:

- **The key was sent to an LLM provider.** Whatever your policy is for that,
  you should know it happened so you can decide whether to rotate.
- **The key is sitting in a file that is easy to share.** Transcripts get
  copied into bug reports, synced to backups and zipped up for teammates.

Most people have hundreds of these sessions. Nobody is going to read them all.

> If you have used an AI coding agent on a real project, there is a good chance
> at least one live credential is sitting in your session history right now.

## What the Credentials Checker does

Agento 1.4.0 adds a **Credentials Checker** under *Security*. It reads through
your Claude Code transcripts in the background and flags anything that looks
like a real credential, including:

- AWS, Google Cloud and Azure keys
- GitHub and Slack tokens, and npm access tokens
- OpenAI and Anthropic API keys
- Stripe live keys
- Database URLs with a password in them
- Private keys and JWTs

Findings are grouped by session, so you can see exactly which conversation a
key leaked in. From there you decide what to do:
rotate the key, mark the finding as a false positive, or whitelist a value or
a whole rule you do not care about.

## See it in action

Here is the whole flow in the real app: switch the checker on, see what it
found, and triage a finding. The sessions and keys are made up for the demo.

<iframe
  class="entry__embed"
  src="/blog/credentials-checker/walkthrough.html"
  title="Walkthrough: turning on the Credentials Checker in Agento and triaging a finding"
  loading="lazy"
></iframe>

## Built so that the checker is not another leak

A tool that hunts for secrets has to be careful not to become the place they
pile up. So:

- **It runs entirely on your machine.** Agento is a desktop app. Your
  transcripts are never uploaded anywhere to be scanned.
- **The raw secret is never stored.** The page shows a masked snippet, enough
  to recognise the key without exposing it again.
- **It favours precision over noise.** The detection rules come from
  [gitleaks](https://github.com/gitleaks/gitleaks), a widely used open source
  secret scanner, and only formats with no plausible innocent reading are
  included. There is no guessing based on random looking strings, so a finding
  is almost always worth your attention.
- **It is off until you turn it on.** Nothing is read until you opt in.

## Try it in two minutes

1. [Install Agento](/docs/installation/) for macOS, Windows or Linux, or update
   an existing install to 1.4.0.
2. Open *Settings → Security* and switch on the Credentials Checker.
3. Go to *Security → Credentials Checker* and watch the findings come in.

New sessions are checked as they happen, so once it is on, you find out about
the next leak the same day instead of never.

If this saves you from a forgotten key, please
[star Agento on GitHub](https://github.com/shaharia-lab/agento). It is free,
open source, and a star is the best way to help other developers find it.
