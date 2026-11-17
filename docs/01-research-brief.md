# Phase 1 — Research & Engineering Brief

Status: decisions are locked unless marked **VERIFY**. Items marked VERIFY could not be
confirmed from primary documentation and have an explicit check in the Wave gates.

## 0. Where the original spec is wrong or optimistic

Read this first. Each point changed the design.

| # | Spec assumption | Reality | Design consequence |
|---|---|---|---|
| 1 | Alerts "within milliseconds of detecting a spread" | In-process latency (listing in → alert out) is sub-millisecond. *Market* latency is the polling interval, bounded below by rate limits: seconds to minutes. Affiliate APIs are catalogue feeds, not tick streams. | We measure and report both: `pipeline_latency_ms` on every opportunity, and observation age. Nobody should size positions as if this were a low-latency feed. |
| 2 | Affiliate cashback (2–8%) boosts your own margin | Affiliate programmes generally void commission on the affiliate's own orders; Shopee is named as having a strict no-self-purchase rule, with invalidated conversions and possible removal as the penalty ([Involve Asia](https://helpcentre.involve.asia/portal/en/kb/articles/can-i-purchase-from-my-own-links)). | `cashback.mode = "disabled"` by default. The term stays in `C_eff` for third-party cashback programmes whose terms allow it, haircut by `realization_bps`. The affiliate API is used as a **data channel** and for link generation. |
| 3 | Percentage vouchers scale with price | On a 30M₫ phone an "8% off" voucher is almost always capped (e.g. 1M₫ ≈ 3.3%). The cap is what binds. | `DiscountKind::Percent` carries a mandatory-to-consider `cap`; examples and tests centre on the capped case. |
| 4 | Vouchers in config are obtainable | 00:00 platform vouchers are limited-quantity and gone in seconds. | `VoucherRule.contested`; every opportunity carries `net_profit_if_contested_fail`; if that is ≤ 0 the alert is capped at `Normal`. |
| 5 | ≥7% net ROI is a routine threshold | Sealed buyback dealers pay below authorized retail. A real 7% net spread on a sealed iPhone through an authorized channel is rare and usually means a stacked contested voucher, a pricing glitch (orders get cancelled), or a stale exit quote. | Expect few `High` alerts. `Watch` tier and `/top_spreads` exist so the operator can see near-misses and calibrate. Glitches are flagged, not celebrated. |
| 6 | "Sealed" is binary | Some chains activate e-warranty at sale. A sealed-but-activated unit gets a lower quote. | `SealedCondition` on quotes, `activates_warranty_at_sale` per source, `RiskFlag::WarrantyActivationRisk`. |
| 7 | JSON-LD price = the price you pay | JSON-LD usually carries the public list/promo price. Member tiers, student prices, trade-in bonuses and payment promos are not in it. | Connectors report `listed_price` only. Everything else is a `VoucherRule`. Divergence between JSON-LD and DOM price is logged, not guessed at. |
| 8 | One unit of profit × N | Flash deals and vouchers are typically limited to one per account/device/payment method. | The engine prices **one unit**. No quantity scaling anywhere. |
| 9 | "Rust 2024 / 1.80+" | Edition 2024 requires 1.85; the code uses let-chains (1.88). | `rust-version = "1.88"`. |
| 10 | `dashmap` for "lock-free state" | Shared concurrent maps are the wrong default here. Throughput is a few requests per second. | Single-owner state inside one pipeline task; `arc-swap` for read-mostly snapshots. `dashmap` is in the workspace but should stay unused unless profiling says otherwise. |

Not legal or tax advice: buying to resell at volume has tax and consumer-terms implications
(retailers may cancel orders they identify as reseller purchases). That is the operator's
call; the system only informs.

## 1. Crate selection

Versions are floors in `[workspace.dependencies]`; `Cargo.lock` is committed at the Wave 0
gate. **VERIFY** at Wave 0: this workspace was authored without registry access, so the
lockfile has not been generated yet and exact patch versions are whatever `cargo
generate-lockfile` resolves.

| Concern | Decision | Why / rejected alternative |
|---|---|---|
| Runtime | `tokio` 1 (multi-thread) | Ecosystem default; `teloxide`, `reqwest`, `sqlx` all assume it. |
| HTTP | `reqwest` 0.13, features `rustls`, `http2`, `json`, `query`, `gzip`, `brotli` | 0.13 made rustls the default backend and renamed the `rustls-tls` feature to `rustls`; `query`/`form` became opt-in ([release notes](https://seanmonstar.com/blog/reqwest-v013-rustls-default/)). One `Client` per source. HTTP/2 matters for politeness (one connection per origin), not speed. |
| Rate limit | `governor` 0.10, one limiter per `SourceId::rate_limit_domain()` | GCRA, `until_ready().await`. A per-connector limiter is wrong: two connectors on one domain would double the load. |
| Retry | `backon` 1 | Exponential + jitter, composable with a retry predicate (`ArbError::is_retryable`). Hand-rolled loops always get jitter or budget wrong. |
| HTML | `scraper` 0.23 | **Gotcha:** `scraper::Html` is `!Send`. Parse and extract inside one synchronous function; never hold it across an `.await`. For pages > ~300 KB use `spawn_blocking`. |
| JSON | `serde_json` | `simd-json` is **not warranted**: payloads are ≤ a few hundred KB at ≤ a few per second; parse time is microseconds against 100+ ms of network. |
| Money | `i64` đồng newtype (`VndAmount`), rates in bps | See §2. `rust_decimal` rejected: VND has no minor unit, so a decimal type adds 128-bit arithmetic and a scale field to represent integers. |
| Time | `chrono` + `chrono-tz` | `Asia/Ho_Chi_Minh` has no DST, but using the tz database keeps intent explicit and survives a config change to another market. |
| Storage | `sqlx` 0.8 + SQLite (WAL) | Single node, single writer, ≤ a few thousand rows/min. Postgres buys nothing until there are multiple writers. Use the runtime `query`/`query_as` API (not `query!`) so `cargo check` needs no database. |
| Dedup cache | `moka` 0.12 | Bounded size with TinyLFU eviction. **Not** used for cooldown timing — see §3.4. |
| Snapshots | `arc-swap` 1 | Hot-swap `VoucherBook`/operator quotes with wait-free reads. |
| Telegram | `teloxide` 0.17 (`macros`, `rustls`, `ctrlc_handler`, `throttle`) | Typed commands, `dptree` dispatch, `Throttle` adaptor for Bot API limits. |
| Config reload | `notify` 8 + explicit `/vouchers` | File watch for local edits; the bot command for remote edits. Both funnel through `ControlPlane::replace_vouchers`. |
| Errors | `thiserror` 2 in libraries, `anyhow` only in `arb-cli` | |
| Observability | `tracing` + `tracing-subscriber` (JSON) | OpenTelemetry export is deferred: with one process and one operator, structured logs plus `/health` counters cover it. Add OTLP behind a feature flag when there is a collector to send to. |
| Tests | `proptest` 1, `wiremock` 0.6, `tempfile` | |
| Async traits | Hand-written `BoxFuture` in `arb-core::ports` | Ports are used as `Arc<dyn Trait>`; native `async fn` in traits is not dyn-compatible. |

## 2. Why `f64` is banned for money (and what the real argument is)

The folk argument — "floats can't represent money" — is weak for VND: every integer below
2^53 is exact in `f64`, and VND has no decimals. The real problems are:

1. **Rates.** Every interesting operation multiplies by 8%, 2.5%, 97%. `0.08` is not
   representable; results land on `…999.9999` or `…000.0001` and the rounding step that
   follows becomes input-dependent. `34_990_001 × 0.08` must be `2_799_200` (floor) by
   rule, not by accident of representation.
2. **Unspecified rounding.** With floats the rounding direction is whatever the last
   cast did. We need a *policy*: discounts round down, costs and haircuts round up, ROI
   floors — so error always understates profit. `VndAmount::mul_bps` takes a mandatory
   `Rounding` argument.
3. **Thresholds are comparisons.** `roi >= 7%` on a float flips on the last ulp; two
   code paths computing the same spread in a different order can disagree on whether to
   alert. Integer bps comparisons are exact and order-independent.
4. **No `Eq`/`Ord`/`Hash`.** The dedup fingerprint hashes a price bucket. A float cannot
   be a map key; NaN poisons sort and max.
5. **Silent failure.** Float overflow gives `inf`, bad input gives `NaN`, both propagate
   quietly. `VndAmount` arithmetic is checked and returns `ArbError::Arithmetic`.

`clippy::float_arithmetic = "deny"` is set workspace-wide. A float may appear only at a
parse boundary (e.g. a JSON `4.9` rating → `rating_milli`) under a local `#[allow]`.

## 3. Concurrency and polling topology

### 3.1 The budget arithmetic that shapes everything

Polite budget for a retailer: ~20 requests/minute per domain. With 50 mapped SKUs on that
domain a full sweep takes 150 s — *regardless* of "burst mode". Burst mode therefore cannot
mean "poll everything every 10 s". It means: **reallocate a fixed budget to the few SKUs
most likely to cross the threshold.**

At 20 rpm and a 10 s burst target, a domain sustains `20 × 10 / 60 ≈ 3` SKUs at burst
cadence. Everything else on that domain is polled with what is left. This number goes in
`/health` so the operator sees what burst actually buys.

### 3.2 Scheduler

- One **dispatcher task per rate-limit domain**. It owns a min-heap of
  `(due_at, priority, sku, mapping)` jobs.
- Loop: pop the most urgent job → `limiter.until_ready().await` → acquire a concurrency
  permit → spawn the fetch → on completion, reschedule the job at `now + interval(mode,
  priority) ± jitter`.
- **Priority = proximity to threshold**: the best ROI seen for that `(sku, source)` in the
  last N minutes; unseen jobs get top priority once. No hand-maintained priority list.
- **Mode** is recomputed from the injected clock on every loop turn: `Burst` iff the ICT
  time is inside a `FlashWindow` *and* the ICT date the window ends on is a campaign day.
- **Shedding**: if a job is overdue by more than one base interval, skip it, count it in
  `polls_shed`, reschedule. Never queue unboundedly behind the limiter.
- Intervals are measured on `tokio::time::Instant` (monotonic); window membership on wall
  time. A wall-clock step cannot stall or stampede the heap.
- At window start, pending low-priority jobs are *not* all pulled forward (that is a
  self-inflicted thundering herd). Only the top-K per domain are.

### 3.3 Pipeline

Fetch results go over a bounded `mpsc` to **one pipeline task** that owns all mutable
state (trailing medians, best-spread table, dedup). Per-listing work is microseconds of
integer math, so a single task is faster than any shared-map design and trivially
deterministic. The pipeline is a synchronous struct (`arb_engine::Pipeline`) wrapped by an
async shell; the replay simulator drives the same struct directly.

Two output buses with different loss semantics:

| Bus | Capacity | When full |
|---|---|---|
| Alert bus (`AlertEvent`) | small (256) | sender awaits — alerts are never dropped |
| Event bus (`EngineEvent`) | large (8192) | `try_send`; drop + count in `events_dropped` |

A slow disk must not delay an alert; a slow Telegram must eventually slow the pipeline
rather than lose alerts.

### 3.4 Dedup and time

Cooldown must use the **injected clock**, not the cache's own TTL: `moka` expires on real
time, which breaks replay determinism and makes tests sleep. Each entry stores
`last_alert_at` and `last_alert_cost`; the pipeline compares against `clock.now()`.
`moka` only bounds memory (`max_capacity`, with a long idle TTL as hygiene).

Re-alert rule for an incoming opportunity with fingerprint `F` and cost `c`:
1. No entry for `(sku, colour, source)` → `New`.
2. `c <= last_cost × (1 − realert_drop_bps)` → `PriceDrop` (even inside cooldown).
3. `now − last_alert_at >= cooldown` → `CooldownExpired`.
4. Otherwise suppress (count it).

The price bucket in the fingerprint stops ±1,000₫ flapping from registering as change.

### 3.5 Shutdown

`CancellationToken` from `arb-cli`. Order: stop dispatchers → drain in-flight fetches
(bounded by source timeout) → pipeline drains its inbox → alert bus drains to Telegram →
storage flushes its batch and checkpoints WAL. A second signal aborts.

## 4. Failure modes

| Failure | Detection | Response |
|---|---|---|
| 502/503/504 during flash sale | `ArbError::HttpStatus` | Retry ≤ 2 with jittered backoff inside the poll's time budget; honour `Retry-After`; count toward breaker. During burst windows retries are *reduced*, not increased — the origin is already struggling. |
| 429 / 403 | status | No retry. Open the breaker immediately for `breaker_open_secs` and halve the source's effective rate until the next clean window. A 403 is a signal to back off, never to evade. |
| Partial HTML (truncated body, skeleton page) | JSON-LD block missing or unparsable | `Parse{stage: JsonLd}` → fall back to catalog JSON → CSS. A CSS-sourced price gets `LowTrustExtraction` and cannot be `High`. If all fail, keep the previous observation and mark it stale. |
| JSON-LD present but `price` is 0 / missing / a range | `check_invariants`, `AggregateOffer` handling | Use `lowPrice` only when `offerCount` semantics match a single variant; otherwise reject as ambiguous. Never substitute a crossed-out price. |
| `OutOfStock` / `PreOrder` / `Discontinued` | `Availability` | Tick is recorded; no opportunity (`NotPurchasable`), `PreOrder` only as a flagged `Normal`. |
| Price glitch (e.g. 3M₫ phone) | below 60% of trailing median | `RejectReason::BelowPriceFloor`; surfaced separately, never as an arbitrage alert. |
| Stale buyback quote | `BuybackQuote::is_stale` | Still priced; `StaleBuybackQuote` flag; capped at `Normal`; counted in `/health`. This is the highest-impact silent failure: a stale exit price fabricates profit. |
| Clock skew | compare local clock with HTTP `Date` headers (median offset per source) | Warn above 2 s. Signed API calls fail outright on skew, so `Auth` errors trigger a skew check first. Run `chrony`/`systemd-timesyncd` on the host; the process does not adjust time. |
| Voucher window boundary | half-open `[from, until)` | A listing observed at 23:59:58 is priced with rules active at its `observed_at`, not at evaluation time. |
| Telegram 429 | `RetryAfter` | `Throttle` adaptor plus honouring `retry_after`; alerts queue on the lossless bus. |
| SQLite busy / disk full | sqlx error | Writer retries the batch; on repeated failure it drops the batch, counts it, and the bot reports degraded storage in `/health`. Alerts continue. |
| Task panic | `JoinHandle` returns `Err` | Supervisor in `arb-cli` logs, restarts the task with backoff, and sends one operator message. Panics are bugs: `unwrap`/`expect`/`panic!` are denied by clippy outside tests. |
| Config edit with a typo | `deny_unknown_fields` + `validate()` | Reload is rejected atomically; the old snapshot stays live; the error text goes back to the operator. |

## 5. Marketplace API notes

- **Shopee Affiliate Open API** — GraphQL at `open-api.affiliate.shopee.vn/graphql`;
  queries such as `productOfferV2`, `shopOfferV2`, `generateShortLink`; credentials are an
  App ID and secret issued in the affiliate dashboard after approval.
  **VERIFY** the signing scheme against the dashboard's own documentation before writing
  the client: the commonly implemented form is an `Authorization: SHA256
  Credential=<appId>, Timestamp=<ts>, Signature=<hex>` header where the signature is
  SHA-256 over `appId + timestamp + body + secret`, but third-party write-ups describe it
  as HMAC-SHA256. The client isolates signing behind one function with a test vector taken
  from the official docs. **VERIFY** the request quota shown in your dashboard and set
  `requests_per_minute` under it.
- **Lazada Open Platform** — REST at `api.lazada.vn/rest`; every call carries `app_key`,
  `timestamp` (ms), `sign_method=sha256`, `sign`. The signature is HMAC-SHA256 keyed by the
  app secret over the API path followed by all parameters sorted by name and concatenated
  as `key+value`, hex-encoded upper-case. **VERIFY** the affiliate-specific endpoint paths
  and which of them require a user `access_token` in the Open Platform console.
- Neither API exposes whether *you* hold a voucher, live flash-sale stock, or per-account
  purchase limits. Those stay operator-supplied (`VoucherBook`) or unknown (risk flags).

## 6. What to measure before trusting it

1. **Replay, not vibes.** Record a campaign day (`record_observations = true`), then
   replay with different thresholds/decay tiers. The question is not "how many alerts" but
   "of the alerts, how many were still purchasable at that price N seconds later".
2. **Decay calibration.** The flat haircut tiers are a prior. Every `/set_exit` update is
   stored as a time series; after one campaign you can measure how far quotes actually
   fell over 1/3/6 days and replace the prior.
3. **Persistence filter.** A price seen in exactly one poll is much more likely to be a
   glitch or cache artefact than one seen in two consecutive polls. The replay tool should
   report alert precision with and without a "seen twice" requirement before that
   requirement is made default.

## Sources

- [reqwest v0.13 — rustls by default](https://seanmonstar.com/blog/reqwest-v013-rustls-default/)
- [reqwest 0.13.0 release notes](https://newreleases.io/project/cargo/reqwest/release/0.13.0-rc.1)
- [teloxide-macros on lib.rs (teloxide 0.17, MSRV)](https://lib.rs/crates/teloxide-macros)
- [Involve Asia: purchasing from your own affiliate links](https://helpcentre.involve.asia/portal/en/kb/articles/can-i-purchase-from-my-own-links)
- [Shopee Affiliate Program terms](https://help.shopee.sg/portal/4/article/77189)
- [Shopee module setup (regional affiliate portals, Open API credentials)](https://ce-docs.keywordrush.com/modules/affiliate/shopee)
- [Lazada signature discussion (HMAC-SHA256 steps)](https://community.make.com/t/text-parsing-using-utf-8-encoding-and-sha256-hashing-for-lazada-signature/22398)
- [lazada-open-api-package (api.lazada.vn, sign_method=sha256)](https://npmjs.com/package/lazada-open-api-package)
