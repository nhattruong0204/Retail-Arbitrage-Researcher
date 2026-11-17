# Prompt O-1 — `arb-engine` (Claude Opus 5.5)

You are implementing one crate in an existing Rust workspace: `crates/arb-engine`.
Other agents are implementing sibling crates in parallel against the same frozen
contracts. Your work will be reviewed line by line for mathematical correctness,
starvation, unbounded growth and panic safety.

## Read first (in this order)

1. `crates/arb-core/src/**` — the frozen contracts. Every shared type, the pricing
   identities (`CostBreakdown::build`, `ExitBreakdown::build`, `net_profit_and_roi`),
   `VoucherRule::discount_on`, `Thresholds::classify`, `DecayConfig::haircut_for`,
   `FlashWindow::contains`, the `PriceConnector` / `ControlPlane` / `Clock` ports.
2. `docs/02-architecture.md` — data flow, task inventory, §5.2 (your pinned facade), §8.
3. `docs/01-research-brief.md` §3 (topology, dedup-and-time) and §4 (failure modes).
4. `config/*.toml` — real examples of every config type.

## Hard rules

- Touch only `crates/arb-engine/**`. Do not edit `arb-core`, root manifests, configs or
  docs. If a contract is wrong or missing, follow `docs/03-orchestration.md` §3 (write
  `crates/arb-engine/CHANGE_REQUEST.md`); do not work around it by redefining a type.
- Dependencies: only crates listed in the root `[workspace.dependencies]`, added as
  `name = { workspace = true }`. You will need at least `tokio`, `tokio-util`, `futures`,
  `governor`, `moka`, `arc-swap`, `chrono`, `chrono-tz`, `unicode-normalization`,
  `tracing`; dev: `proptest`, `tokio` with `test-util`.
- Edition 2024, MSRV 1.88. `thiserror`-style errors via `arb_core::ArbError` only.
- No `unwrap`/`expect`/`panic!`/`todo!` outside `#[cfg(test)]` (clippy denies them).
- No floating point anywhere. No `as` casts that can truncate; use `try_from`.
- Never read the system clock. Wall time comes from the injected `Arc<dyn Clock>`;
  intervals use `tokio::time` (so tests run under `tokio::time::pause()`).
- Every collection and channel is bounded, with a stated eviction rule.
- All money math goes through `arb-core`. Do not re-implement `C_eff` or `P_exit`.

## Public API (binding, verbatim)

Implement exactly the facade in `docs/02-architecture.md` §5.2: `EngineDeps`,
`EngineHandle`, `spawn`, `Pipeline`, `StepOutput` with the listed methods and signatures.
You may add further public items; you may not change these.

## What to build

### 1. `Pipeline` — the synchronous, deterministic core

`ingest(requested_sku, listing, now)` runs these stages and returns the events and at
most one alert. It performs no I/O, reads no clock, uses no randomness.

**Stage 0 — record.** If `channels.record_observations`, emit
`EngineEvent::Observation` first, before any filtering.

**Stage 1 — resolve the SKU.**
- If `listing.sku_hint` is set (curated mapping), trust it; it must equal
  `requested_sku`, otherwise `ArbError::Internal`.
- Otherwise fuzzy-match the title against the requested SKU only (never search the whole
  catalog — that is how accessories get matched to phones): every `required_tokens` entry
  must be present, no `forbidden_tokens` entry may be present. Failures are
  `MissingRequiredToken` / `ForbiddenToken`.
- Resolve colour: `color_hint`, else the unique `ColorVariant` whose alias appears in the
  title. Two matches → `AmbiguousColor`. No match → colour `None` (allowed).

**Title normalization** (write it once, test it hard): Unicode NFC; lowercase; map
punctuation to spaces but keep `%`, `/`, `+`, `-` inside tokens; collapse whitespace;
join a number with a following storage unit (`256 gb` → `256gb`, `1 tb` → `1tb`).
**Keep Vietnamese diacritics** — `cũ` (used) must not collide with `củ` or `cu`.
Token matching is on token boundaries: `lock` must not match `unlocked`, `99%` must not
match `199%`. Multi-word tokens (`like new`, `cường lực`) match as consecutive tokens.

**Stage 2 — negative-keyword and attribute gate** (applies to mapped listings too):
- any `filter.negative_tokens` hit → `NegativeKeyword { token }`;
- `condition` is `Used`/`Refurbished` → `ConditionNotNew`;
- `filter.require_authorized_seller` and `!seller.tier.is_authorized()` →
  `UnauthorizedSeller`;
- `!availability.is_purchasable()` and not `PreOrder` → `NotPurchasable`
  (still emit a `Tick` so history shows the stock-out).

**Stage 3 — price-floor sanity.** Maintain a trailing window of accepted listed prices
per SKU (`median_window`, ring buffer). Median = window median once it holds
`median_min_samples`, else `CanonicalSku::retail_benchmark`. If
`listed_price < median × price_floor_ratio_bps` → `BelowPriceFloor`; **do not** add the
price to the window (an outlier must not drag the median toward itself). Accepted prices
are added *after* the check. The median must be O(log n) or better per update — not a
sort per listing.

**Stage 4 — effective cost.** Rules considered: `VoucherBook::active_at(listing.observed_at)`
filtered by `is_eligible(source, sku, category, seller.tier == OfficialMall)`; payment
rules additionally require `payment_method ∈ available_payment_methods`.
Stacking: layers in `VoucherLayer` order (`Shop → Platform → Payment → Shipping`), at
most one rule per layer. The basis for a rule is the listed price (`ListedPrice`) or the
running subtotal after earlier layers (`AfterPriorLayers`). `Shipping` rules discount
the shipping fee only. Because layers interact through `AfterPriorLayers`, greedy
per-layer choice is not always optimal: enumerate the combinations (≤ a handful of
eligible rules per layer) and take the minimum `C_eff`; break ties by fewest contested
rules, then by rule id, so the result is deterministic and independent of input order.
Shipping: `listing.shipping_fee`, else `SourceConfig::default_shipping_fee`.
Cashback per `CashbackConfig`: `Disabled` → 0; `ListedCommission` → listing commission;
`Fixed` → `fixed_bps`; only for configured sources; computed on the post-voucher item
subtotal, rounded `Down`, capped, then multiplied by `realization_bps` rounded `Down`.
Build with `CostBreakdown::build`. Then repeat the whole stacking with contested rules
excluded to get the uncontested cost.

**Stage 5 — exit price.** Quote selection for `(sku, colour)`, in priority order:
operator quote → colour-specific catalog quote → base catalog quote. Among equals, pick
the **lowest** price (worst-case liquidity baseline). No quote → `Tick` with
`effective_cost` set and `exit_price = None`, no opportunity. Colour multiplier: `10000`
for a colour-specific quote; the variant's `liquidity_bps` for a base quote with a known
colour; the **minimum** `liquidity_bps` across the SKU's colours when the colour is
unknown. Invoice expected iff the effective VAT status (listing value, or the source
default when `Unknown`) is `Available`. Decay from
`DecayConfig::haircut_for(listing.delivery or source default)`. Build with
`ExitBreakdown::build`.

**Stage 6 — classify.** `net_profit_and_roi`, `Thresholds::classify`. Derive risk flags
(every `RiskFlag` variant has a defined trigger — document each in code). If any flag
`blocks_high_priority()`, cap `High` at `Normal`. `pipeline_latency_ms = now − observed_at`
(clamped at 0). Emit `Tick` always; emit `Spread` when net profit is positive; update the
best-spread table (one entry per `(sku, colour, source)`, replaced on every evaluation,
removed when the spread turns non-positive or the item becomes unpurchasable).

**Stage 7 — dedup** (only for `High`/`Normal`). Implement exactly the four-step rule in
`docs/01-research-brief.md` §3.4. Keyed by `(sku, colour, source)` holding
`last_alert_at`, `last_alert_cost`, last fingerprint. Cooldown uses `now`, never cache
TTL. `moka::sync::Cache` bounds memory (`max_entries`). Count suppressions.

### 2. Scheduler

Per `docs/01-research-brief.md` §3.1–3.2. One dispatcher task per distinct
`SourceId::rate_limit_domain()` among enabled sources with a connector. Jobs: one per
`(enabled sku, mapping)`; for a marketplace source with no mapping for a SKU, one
`Search` job per `search_queries` entry if `capabilities().supports_search`.
- Mode: `Burst` iff ICT wall time (from `Clock`, converted with `arb_core::ICT`) is in a
  `FlashWindow` and the ICT calendar date on which that window **ends** is a campaign day.
- Engine-side `governor` limiter per domain (`requests_per_minute`, `burst`) and a
  semaphore (`max_concurrency`). Connectors rate-limit too; yours is what makes
  scheduling decisions honest.
- Priority feedback from the pipeline (best recent ROI per job); shedding with a counter;
  per-job exponential backoff on failure so one dead item cannot eat the domain budget;
  honour `HttpStatus::retry_after_secs`; treat `CircuitOpen` as "park the domain until
  `retry_at`".
- Jitter: deterministic given a seed (inject the RNG; tests fix the seed).
- `FetchOutcome::NotModified` → no pipeline work; just reschedule.

### 3. Async shell and `ControlPlane`

`spawn` validates configs, builds the pipeline, starts dispatchers and one pipeline task,
returns `EngineHandle`. Inbox: bounded `mpsc`. Alert bus: `send().await` (lossless).
Event bus: `try_send`, drop and count on full. `ControlPlane` is implemented by sending
commands to the pipeline task over a bounded channel with `oneshot` replies (single
owner; no shared mutable maps). `watch`/`unwatch` add/remove jobs. `health()` aggregates
connector `health()` snapshots, mode, counters, stale-quote count.
Shutdown: on cancellation stop dispatching, let in-flight fetches finish (bounded by
source timeout), drain the inbox, close the buses, then resolve `task`.

## Tests (required)

Property tests (`proptest`, must pass with `PROPTEST_CASES=2000`):
1. `ingest` never panics and never returns `Internal` for arbitrary well-formed listings.
2. Adding an eligible voucher never increases `C_eff`.
3. Increasing `listed_price` (all else equal) never increases `net_profit`.
4. Increasing the decay haircut never increases `exit_price`.
5. `net_profit_if_contested_fail ≤ net_profit`.
6. Shuffling the order of rules in the `VoucherBook` does not change the result.
7. Determinism: the same `(state, sequence)` twice yields identical `StepOutput`s.
8. Dedup: within the cooldown and without a qualifying drop, at most one alert per
   `(sku, colour, source)`; a drop ≥ `realert_drop_bps` always re-alerts.
9. Normalization is idempotent; negative tokens match only on token boundaries.

Example-based tests with hand-computed expected values (show the arithmetic in comments):
capped percent voucher; `after_prior_layers` stack where greedy ≠ optimal; no-invoice
penalty; unknown colour uses minimum liquidity; stale quote caps priority; price-floor
rejection does not move the median; midnight window on 10 Nov 23:59 ICT is `Burst`, on
9 Nov 23:59 is `Baseline`.

Scheduler tests under `tokio::time::pause()` with fake connectors and a fake clock:
24 simulated hours with no per-domain rate violation; mode transitions at exact window
edges; shedding under an impossible budget; a failing job backs off; shutdown completes
and all tasks join.

## Deliverable

- The crate, passing `cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test -p arb-engine`.
- `crates/arb-engine/README.md`: module map; the trigger for every `RiskFlag`; every
  bounded structure with its bound; any behaviour where you had to choose between
  readings of this prompt, and which you chose.
- A final message listing: what you did not finish, every assumption, every
  `CHANGE_REQUEST.md` item.
