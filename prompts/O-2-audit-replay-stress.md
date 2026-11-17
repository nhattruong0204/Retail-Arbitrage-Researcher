# Prompt O-2 — Integration Audit, Replay Simulator, Stress Suite (Claude Opus 5.5)

The workspace is feature-complete: `arb-core` (frozen contracts), `arb-engine`,
`arb-connectors`, `arb-storage`, `arb-notify`, `arb-cli`. Your job is to find what is
wrong with it, prove what is right about it, and build the tool that tells the operator
whether the strategy works at all. You did not write most of this code; assume nothing
about it is correct until you have checked.

## Read first

1. `docs/01-research-brief.md` (all of it — §0 and §6 define what "working" means).
2. `docs/02-architecture.md` (§5 facades, §8 invariants).
3. `docs/03-orchestration.md` §5 (the review protocol — you execute it across crates).
4. Every crate's source and README, including the "assumptions" and "unverified" lists.

## Ownership

You may create or edit only: `crates/arb-cli/src/replay/**`, `crates/arb-cli/tests/**`,
and `AUDIT.md` at the repository root. You may add `[dev-dependencies]` and
dependencies needed by `replay` to `crates/arb-cli/Cargo.toml` (workspace-listed crates
only). **You do not fix bugs in other crates.** You report them in `AUDIT.md` with a
failing test in `crates/arb-cli/tests/` where one can demonstrate the problem; mark such
tests `#[ignore = "AUDIT-<id>"]` so the gate stays green and the finding stays executable.

## Part 1 — Audit (`AUDIT.md`)

Run the full review protocol (sections A–G) against every crate. For each finding:
id, severity (`blocker` / `major` / `minor`), file:line, what is wrong, how to reproduce
(test name or command), suggested fix. Also record what you checked and found sound —
an audit that lists only defects cannot be distinguished from one that did not look.

Specifically hunt for:

- **Money**: any path that computes a cost, discount, exit or ROI without going through
  `arb-core`; any rounding in the optimistic direction; any `as` cast on an amount;
  voucher validity judged at the wrong instant; a `Percent` rule applied without its cap;
  `min_spend` checked against the wrong basis; cashback applied when `mode = disabled`.
- **Independent recomputation**: write a second, deliberately naive implementation of
  the spread (in the test crate, ~50 lines, `i128` arithmetic, brute-force voucher
  enumeration) and differential-test it against `Pipeline::ingest` with `proptest` over
  random catalogs, voucher books and listings. Any disagreement is a finding.
- **Time**: wall-clock reads outside `arb-cli`; cooldown or validity depending on real
  time; the midnight window armed on the wrong date; behaviour at exactly `valid_until`,
  exactly window `end`, and across 23:59:59 → 00:00:00 ICT.
- **Concurrency**: lock held across `.await`; unbounded channel/collection; spawned task
  with no owner; `select!` that can lose a message on cancellation; pipeline blocked by
  the event bus; alert dropped on any path; dispatcher starvation by one failing job;
  shutdown paths that can hang (await on a channel whose sender is never dropped).
- **Failure containment**: what happens to the process when a connector returns garbage,
  SQLite is read-only, Telegram is unreachable for 10 minutes, the voucher file is
  replaced with invalid TOML mid-run, the clock jumps by an hour.
- **Compliance**: any request that bypasses the limiter; any header beyond the configured
  User-Agent and standard conditional/accept headers; retries on 403/429; any endpoint
  that is not a public product page or a documented affiliate/open API.
- **Contract drift**: look-alike types shadowing `arb-core` ones; facades that deviate
  from `docs/02-architecture.md` §5; unresolved `CHANGE_REQUEST.md` files.

## Part 2 — Replay / backtest simulator (`arb replay`)

Replace the stub in `crates/arb-cli/src/replay/`. It drives `arb_engine::Pipeline`
directly — no network, no Telegram, no wall clock.

```
arb replay --db <path> --from <rfc3339> --to <rfc3339>
           --engine <path> --skus <path> --vouchers <path>
           [--set thresholds.min_roi_bps=500 ...]     # override any scalar config key
           [--persistence <n>]                        # require n consecutive sightings
           [--sweep thresholds.min_roi_bps=300:1000:100]
           [--out <dir>]                              # report.json, alerts.csv, sweep.csv
```

- Input: `Store::observations(from, to)` streamed in order. Simulated time is each
  observation's `observed_at` (plus a configurable fixed pipeline delay). Operator quotes
  are applied from `operator_quote_history` at their `quoted_at`, so the exit price at
  each instant is what the operator knew then — no look-ahead.
- Also accept `--fixture <file.jsonl>` (one `{requested_sku, listing}` per line) so the
  tool works without a database; ship a small synthetic fixture session under
  `crates/arb-cli/tests/fixtures/` that includes a flash-sale price drop, a glitch price,
  an accessory listing, a stock-out, a contested-voucher window and a quote update.
- Output must be **byte-identical** across runs for the same inputs (sorted maps, no
  timestamps of "now", no hash-order dependence).

Report metrics, overall and per SKU / source / hour-of-day (ICT):
1. Alerts by priority and kind; suppressed count.
2. **Persistence**: for each alert, how long the item stayed at or below the alerted
   effective cost and purchasable (from subsequent observations): median, p10, p90, and
   the share that lasted < 30 s, < 2 min, < 10 min. An alert whose price is gone before
   a human can check out is a false positive, whatever the ROI said.
3. **Realized-exit sensitivity**: net profit recomputed with the quote in force 1, 3 and
   6 days *after* the alert (when history covers it), versus the decay haircut that was
   applied — this is the calibration check for `DecayConfig`.
4. Contested dependence: share of alerts with `net_profit_if_contested_fail ≤ 0`.
5. Rejections by reason; top negative tokens; listings that passed all filters but look
   suspicious (price within 5% of the floor).
6. Latency: observation age distribution at evaluation (from recorded data).
7. Sweep mode: the above headline numbers per parameter value, as CSV.

Honest-reporting requirements: state coverage (observations per SKU/source and the
largest gap between observations); refuse to print persistence statistics for a SKU with
fewer than 30 observations in the window; never extrapolate beyond recorded data; label
synthetic-fixture results as synthetic in the report header.

## Part 3 — Concurrency and stress suite (`crates/arb-cli/tests/`)

All tests offline, deterministic, under `tokio::time::pause()` with a fake clock and
in-process fake `PriceConnector`s whose behaviour is scripted (latency, error schedule,
price path). Fixed RNG seeds.

1. **Flash-sale storm**: 200 SKUs × 7 sources, all prices drop at simulated 00:00:00 on
   a campaign day. Assert: no per-domain rate-limit violation (count requests per
   virtual minute at the fake connectors); every qualifying drop produces exactly one
   alert; alert order is consistent with observation order per SKU.
2. **Backpressure**: Telegram sink blocked for 10 virtual minutes. Assert: no alert
   lost; pipeline stalls rather than drops; event bus drops are counted and equal the
   shortfall in the store; memory-bearing structures stay within their bounds (expose
   sizes through `/health` or test hooks — if no hook exists, that is a finding).
3. **Failure injection**: one source returns 503 for 5 minutes then recovers; one
   returns malformed bodies forever; one times out. Assert breaker state transitions,
   that healthy sources keep their cadence, and recovery without restart.
4. **Dedup under flapping**: a price oscillating across a bucket boundary every poll
   for an hour produces a bounded number of alerts consistent with the cooldown.
5. **Shutdown**: cancel at 50 random virtual instants (seeded) during the storm; every
   run joins all tasks within the deadline, loses no already-emitted alert, and leaves a
   consistent database (re-open, integrity check, counts reconcile).
6. **Soak**: 24 virtual hours at nominal rate; structure sizes at hour 24 equal sizes at
   hour 2 (±bounds) — the leak test.
7. **Determinism**: the storm run twice yields identical alert sequences.

## Deliverable

- `AUDIT.md`; the replay tool; the test suite. Gate commands pass (ignored tests aside).
- Final message: the five findings you consider most dangerous, with reasons; what you
  could not test and why; whether, on the evidence available, you would trust this
  system's `High` alerts with real money — and what would change your answer.
