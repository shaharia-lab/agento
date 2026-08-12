# Model pricing catalog

Agento estimates Claude Code session cost from the pricing catalog in
`internal/pricing/`: a SQLite table (`model_pricing`) of effective-dated rates,
seeded from the embedded `catalog.json` on startup.

Cost is computed per assistant message, at that message's own model and
timestamp, and stored on the session — see
[Claude Sessions → Cost](claude-sessions.md#cost) for what is done with it.

## Why effective dating

A rate row carries an `effective_from` timestamp, and cost lookups take
`(model_id, spent_at)` — the newest rate whose `effective_from <= spent_at`
wins. Adding a rate never rewrites an old one, so last month stays priced at
last month's rate. This is what makes historical cost figures immutable when a
provider changes its prices (e.g. Claude Sonnet 5's introductory $2/$10 per
MTok through 2026-08-31 reverting to $3/$15 on 2026-09-01: two rows, one
boundary, both halves of the year correct).

## Maintaining rates from the UI

**Settings → Model Pricing** lists every model the catalog knows, grouped by
provider, with the rate in force now. Expanding a model shows its full rate
history — every rate with the date it took effect and the source it came from —
so a past cost figure can be traced to the rate that produced it.

Two actions, and the difference matters more than anything else on the page:

- **Add rate** is what you want when a provider *changed its price*. It appends
  a row with a new effective date and leaves every earlier rate untouched, so
  usage already costed keeps the price it was charged at. The form states which
  usage window the new rate will govern before you submit.
- **Correct rate** is only for a value *entered in error*. It edits a row in
  place and therefore rewrites costs already reported for that rate's window.

These are separate endpoints, not one upsert: adding refuses to overwrite an
existing rate (it returns the colliding row so the UI can offer to correct it
instead), and correcting refuses to create one. Reaching for "correct" when the
price merely changed is how a user silently rewrites their own history, and the
API is shaped to make that hard.

A rate can also be **deleted** — for a row that should never have existed. That
leaves the usage it covered priced by whatever earlier rate now wins, or unpriced
if there is none.

Models seen in your sessions with no rate at all appear at the top of the tab —
their tokens are excluded from every cost total until priced, so that list is
the tab's most useful starting point.

Saving any change bumps the catalog revision, which invalidates the cached
session costs; they recompute on the next scan — a **full re-read of every
transcript**, because re-pricing needs each message's own model and timestamp.
On a large corpus that takes a few minutes. Rows you edit are marked
user-modified and survive upgrades untouched.

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

## Context-length rate tiers

Some providers charge more for a long request. A rate can therefore carry
**bands**, each keyed by an inclusive upper bound on the request's input tokens:

```json
"tiers": [
  { "max_input_tokens": 256000,  "input": 0.40, "output": 1.60, "cache_read": 0.04 },
  { "max_input_tokens": 1000000, "input": 1.20, "output": 4.80, "cache_read": 0.12 }
]
```

Four things about how bands behave:

- **A band is selected, not accumulated.** Alibaba bills every token of a request
  at the chosen band's rate, so there is no progressive-bracket arithmetic. A
  request larger than every declared bound uses the highest band.
- **The boundary counts all input-side tokens** — fresh input plus cache reads
  plus cache writes. The providers state how cached tokens are billed but not
  whether they count toward the bound; this reading is documented in the code and
  changed only there.
- **A rate with no bands is unaffected.** The flat arithmetic did not move; an
  empty band list simply skips band selection.
- **The UI shows bands read-only.** Editing them is deliberately not offered —
  and because a band is picked *before* any price is applied, correcting a tiered
  rate by hand would otherwise save and then change nothing at any request size.
  Correcting a rate therefore **clears its bands** and makes it flat: your value
  wins, which is the same rule `user_modified` encodes elsewhere.

Bands live in their own table, so the `UNIQUE(model_pattern, effective_from)`
identity — and with it the add-vs-correct semantics above — is untouched. The
catalog revision hashes them, so a band correction re-prices stored costs like
any other change.

## Non-Anthropic providers

Claude Code is routinely pointed at Anthropic-API-compatible backends, so the
catalog is not Anthropic-only. Seeded as of 2026-08-09, International endpoints,
USD per million tokens:

| Provider | Pattern | Input | Output | Cache read | Context bands |
|---|---|---|---|---|---|
| Moonshot | `k3` (exact), `kimi-k3` | 3.00 | 15.00 | 0.30 | — |
| Z.ai / Zhipu | `glm-5.2` | 1.40 | 4.40 | 0.26 | — |
| Alibaba | `qwen3.7-max` | 2.50 | 7.50 | 0.25 | — |
| Alibaba | `qwen3.7-plus` | 0.40 | 1.60 | 0.04 | ≤256K, then 1.20 / 4.80 |
| Alibaba | `qwen3.6-flash` | 0.25 | 1.50 | 0.025 | ≤256K, then 1.00 / 4.00 |
| Alibaba | `qwen3.5-397b-a17b` | 0.60 | 3.60 | 0.06 | — |
| Alibaba | `qwen3.5-plus` | 0.40 | 2.40 | 0.04 | ≤256K, then 0.50 / 3.00 |
| Alibaba | `qwen3.5-flash` | 0.10 | 0.40 | 0.01 | — |
| Alibaba | `qwen3-max` | 1.20 | 6.00 | 0.12 | ≤32K, ≤128K 2.40 / 12.00, ≤256K 3.00 / 15.00 |

Notes for the next refresh:

- Moonshot's canonical ID is `kimi-k3`, but Claude Code records the bare `k3`
  in transcripts, so both are seeded. `k3` is an **exact** match on purpose — a
  two-character prefix would swallow any future ID starting `k3`.
- **Alibaba prices caching as a percentage rule**, not per model: explicit-cache
  creation is 125% of input and hits are 10%. Their rows encode that, which is
  also why their 1-hour column is 1.25× rather than the Anthropic 2×.
- **Band bounds use the provider's own K/M labels read as decimal** (256K =
  256,000). The pricing pages do not say whether K means 1000 or 1024, and
  inventing the binary reading would be precision they never published.
- Several Alibaba models carry a "Limited-time X% off" against a separate list
  price. Where an expiry is published, capture it with an `effective_from` so
  the reversion does not silently misprice history; where none is published, no
  boundary row is seeded rather than guessing one.
- **Qwen3.8-Max is deliberately absent.** It appears on Alibaba's marketplace
  but on no pricing page, with two competing IDs in circulation and no
  published rate — seeding it would be a guess. A missing row costs nothing and
  is visible in the unknown bucket; a wrong row is invisible.
