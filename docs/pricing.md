# Model pricing catalog

Agento estimates Claude Code session cost from the pricing catalog in
`internal/pricing/`: a SQLite table (`model_pricing`) of effective-dated rates,
seeded from the embedded `catalog.json` on startup.

## Why effective dating

A rate row carries an `effective_from` timestamp, and cost lookups take
`(model_id, spent_at)` — the newest rate whose `effective_from <= spent_at`
wins. Adding a rate never rewrites an old one, so last month stays priced at
last month's rate. This is what makes historical cost figures immutable when a
provider changes its prices (e.g. Claude Sonnet 5's introductory $2/$10 per
MTok through 2026-08-31 reverting to $3/$15 on 2026-09-01: two rows, one
boundary, both halves of the year correct).

## Maintaining rates

- **Add a row when a price changes** — same `model_pattern`, new
  `effective_from`. Do not edit the old row: history was priced against it.
- **Editing in place** is reserved for correcting a rate that was *wrong when
  entered* (a typo, a misread page). Edits mark the row `user_modified` and
  re-price the affected history — that is the intent, but be sure that's what
  you want.
- **Cache columns default to Anthropic's TTL multipliers** — cache-write 5m =
  1.25× input, 1h = 2× input, cache-read = 0.1× input — and each can be
  overridden per rate with `cache_write_5m`, `cache_write_1h` and `cache_read`.
  Other providers publish their own cached-input price and generally do not
  split cache writes by time-to-live, so their rows state all three.
- Every row records its `source` (the provider pricing page and the date it
  was checked) so the next maintainer can verify it. For a non-Anthropic
  provider the `source` must also name **which endpoint** the rate reflects:
  Alibaba's Mainland China endpoint is 60–70% cheaper than International, and
  Z.ai's OpenRouter-hosted rate differs from direct.

Built-in rows re-seed idempotently on startup; a row you modified survives
upgrades untouched. Rate changes bump a content hash of the catalog, which the
session cache notices on its next refresh — costs recompute from the new
rates, no cache wipe required. Insights (stored per-session costs) follow
within the insight worker's 5-minute sweep.

## Model matching

Exact match first, then longest prefix — so dated snapshots
(`claude-haiku-4-5-20251001`) and context variants (`claude-opus-4-7[1m]`)
resolve to their family's rate. A model with no matching row contributes no
cost rather than being priced at another model's rate; the unknown tokens are
counted and surfaced separately in analytics.

Two flags qualify a match:

- **`billable: false`** marks a model that genuinely costs nothing — Claude
  Code's `<synthetic>` placeholder, embedding models. It resolves, so it prices
  at $0.00 *without* landing in the unknown-pricing bucket. This is the only
  way to seed all-zero rates: elsewhere a zero is treated as an unfilled entry
  and rejected at parse time, so a half-filled row fails the build instead of
  silently under-reporting spend.
- **`estimated: true`** marks a rate that is a best effort rather than a
  published price. The bare family aliases (`opus`, `sonnet`, …) name no
  concrete model, so they are priced at that tier's current flagship and say
  so. The resolver also sets this when usage predates every row for a pattern.

## Non-Anthropic providers

Claude Code is routinely pointed at Anthropic-API-compatible backends, so the
catalog is not Anthropic-only. Seeded as of 2026-08-09, International endpoints:

| Provider | Pattern | Input | Output | Cache read |
|---|---|---|---|---|
| Moonshot | `k3` (exact), `kimi-k3` | 3.00 | 15.00 | 0.30 |
| Z.ai / Zhipu | `glm-5.2` | 1.40 | 4.40 | 0.26 |
| Alibaba | `qwen3.5-397b-a17b` | 0.60 | 3.60 | 0.06 |
| Alibaba | `qwen3.5-plus` | 0.40 | 2.40 | 0.04 |
| Alibaba | `qwen3.5-flash` | 0.10 | 0.40 | 0.01 |
| Alibaba | `qwen3-max` | 1.20 | 6.00 | 0.12 |

Notes for the next refresh:

- Moonshot's canonical ID is `kimi-k3`, but Claude Code records the bare `k3`
  in transcripts, so both are seeded. `k3` is an **exact** match on purpose — a
  two-character prefix would swallow any future ID starting `k3`.
- **Alibaba prices caching as a percentage rule**, not per model: explicit-cache
  creation is 125% of input and hits are 10%. Their rows encode that, which is
  also why their 1-hour column is 1.25× rather than the Anthropic 2×.
- **Alibaba tiers by context length** and the catalog has no context dimension.
  The base tier is seeded; `qwen3.5-plus` costs $0.50/$3.00 above 256K and
  `qwen3-max` $2.40/$12.00 above 32K, so long-context usage under-reports.
- Several Alibaba models carry a "Limited-time X% off" against a separate list
  price. Capture those with an `effective_from` so the expiry does not silently
  misprice history.
- **Qwen3.8-Max is deliberately absent.** It appears on Alibaba's marketplace
  but on no pricing page, with two competing IDs in circulation and no
  published rate — seeding it would be a guess. A missing row costs nothing and
  is visible in the unknown bucket; a wrong row is invisible.
