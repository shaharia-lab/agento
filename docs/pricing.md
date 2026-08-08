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
- **Cache columns are derived**, not hand-entered: cache-write 5m = 1.25×
  input, 1h = 2× input, cache-read = 0.1× input.
- Every row records its `source` (the provider pricing page and the date it
  was checked) so the next maintainer can verify it.

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
