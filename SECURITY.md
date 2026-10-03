# Security Policy

We take security seriously and appreciate your help in keeping Agento safe for everyone.

Agento starts agent CLIs with shell access and stores integration credentials, so a
vulnerability described in public can be used against every install before a fix ships.
**Please report every suspected vulnerability privately** — do not open a public issue,
pull request or discussion for it, whatever its severity.

## Reporting a Vulnerability

1. **Use GitHub's private form:**
   [Report a vulnerability](https://github.com/shaharia-lab/agento/security/advisories/new).
   It opens a private thread that only you and the maintainers can read.
2. **If you cannot use the form** — you have no GitHub account, or the form is not
   available to you — email **hello@shaharialab.com** instead.

You do not need to decide how serious the finding is before choosing a route. If you are
unsure whether something is a vulnerability at all, report it privately; we will tell you
if it belongs in the public tracker.

Please include:

- A description of the vulnerability.
- Steps to reproduce or a proof of concept.
- The potential impact and affected components.
- The Agento version and operating system.
- Any suggested fixes, if you have them.

## What to Expect

- We aim to acknowledge your report **within 7 days**.
- We will work with you in the private thread to understand the scope and coordinate a
  fix before anything is made public.
- Fixes are disclosed through
  [GitHub Security Advisories](https://github.com/shaharia-lab/agento/security/advisories),
  published once a fixed release is available.

## What Is Fine to File Publicly

General hardening suggestions that carry **no exploit detail** — a stricter default, an
extra safeguard, a documentation improvement — are welcome as
[regular issues](https://github.com/shaharia-lab/agento/issues). If describing the idea
would require explaining how to attack an existing install, it is a vulnerability report:
use the private form above.

## Supported Versions

Security fixes are applied to the latest release. We recommend always running the most recent version of Agento.
