# Prompt S-2 — `arb-storage`, `arb-notify`, `arb-cli` (Claude Sonnet 5.5)

You are implementing three crates in an existing Rust workspace: SQLite persistence, the
Telegram bot, and the binary that wires everything together. Work in two stages:

- **Stage A** (start immediately): `arb-storage` and `arb-notify`. They depend only on
  `arb-core`.
- **Stage B** (after the orchestrator confirms `arb-engine` and `arb-connectors` are
  merged): `arb-cli`. Until then, do not write code that depends on those crates beyond
  the pinned facades.

## Read first

1. `crates/arb-core/src/**` — especially `opportunity.rs` (`ArbitrageOpportunity`,
   `AlertEvent`, `AlertKind`, `EngineEvent`, `RiskFlag`, `CostBreakdown`, `ExitBreakdown`),
   `ports.rs` (`ControlPlane`, `HealthReport`), `money.rs` (`Display` impls), `voucher.rs`.
2. `docs/02-architecture.md` — §5 (pinned facades of all crates), §6 (config and
   secrets), §7 (tables), §3 (task inventory).
3. `docs/01-research-brief.md` §3.3, §3.5, §4.
4. `docs/03-orchestration.md` §2 (ownership, including the `replay` seam you must leave).

## Hard rules

- Touch only `crates/arb-storage/**`, `crates/arb-notify/**`, `crates/arb-cli/Cargo.toml`
  and `crates/arb-cli/src/**` (not `src/replay/**` beyond the stub described below, and
  not `crates/arb-cli/tests/**`). Contract problems → `CHANGE_REQUEST.md`.
- Dependencies only from root `[workspace.dependencies]` via `{ workspace = true }`.
- `thiserror`/`ArbError` in the two libraries; `anyhow` only in `arb-cli`.
- No `unwrap`/`expect`/`panic!` outside tests. No floats. Money is `VndAmount`; in SQL it
  is `INTEGER`.
- `sqlx`: use the runtime `query`/`query_as` API with `.bind()`, not the `query!` macros,
  so the workspace compiles with no database present. Migrations via `sqlx::migrate!`
  from `crates/arb-storage/migrations/`.
- Secrets come from the environment only and never reach logs, `Debug`, or Telegram.
- No bot command may place orders, open carts, or call a marketplace. The bot informs.

## Stage A.1 — `arb-storage`

Implement the pinned read facade in `docs/02-architecture.md` §5.3 verbatim, plus the
write side (your design; suggested shape below).

- `Store::open`: `SqliteConnectOptions` with `create_if_missing`, `journal_mode = WAL`,
  `synchronous = NORMAL`, `busy_timeout = 5 s`, `foreign_keys = ON`. Two pools: a
  **single-connection** write pool and a small read pool.
- Schema per `docs/02-architecture.md` §7. Timestamps `INTEGER` Unix ms UTC. JSON columns
  are `serde_json` of the `arb-core` type. Indexes: `price_ticks(sku_id, observed_at)`,
  `observations(observed_at)`, `alerts(sent_at)`, `spreads(detected_at)`.
- Writer task:
  `pub fn spawn_writer(store: Store, events: mpsc::Receiver<EngineEvent>, shutdown: CancellationToken) -> JoinHandle<Result<(), ArbError>>`.
  Batches up to 500 events or 250 ms, whichever first, into one transaction. On a failed
  batch: retry once, then drop the batch, increment a counter exposed via
  `Store::stats()`, and continue. On shutdown: drain the channel, flush, then
  `PRAGMA wal_checkpoint(TRUNCATE)`.
- Rejections are sampled: at most one row per `(reason kind, source, platform_item_id)`
  per hour (in-memory bounded map).
- Alert audit: `record_alert(&AlertEvent) -> i64` (status `pending`) and
  `mark_alert(id, DeliveryStatus, telegram_message_id, error)`.
- Overrides (append-only): `save_operator_quote`, `latest_operator_quotes`,
  `save_voucher_book`, `latest_voucher_book`.
- Retention: `prune(older_than)` for ticks/observations/rejections; never prunes alerts
  or operator history.

Tests (`tempfile` DBs): migrations on an empty DB; round-trip of every event variant
with full equality on the deserialized `arb-core` values; `observations()` ordering and
half-open range; batch boundary behaviour under `tokio::time::pause()`; shutdown flushes
everything; a poisoned batch does not stop the writer; `VndAmount` extremes round-trip.

## Stage A.2 — `arb-notify`

### Rendering (pure, no bot, no clock)

`render_alert(&AlertEvent) -> RenderedAlert` exactly as pinned in §5.4. MarkdownV2 with
**every** dynamic string escaped (`_ * [ ] ( ) ~ \` > # + - = | { } . !` and backslash);
URLs go only into buttons, never into message text. The card, top to bottom:

1. Priority + kind line (`High` vs `Normal`; `New` / `PriceDrop` with the previous cost /
   `CooldownExpired`).
2. SKU display name · storage is already in the name · colour display (or "màu ?") ·
   source display name with a Mall / Authorized badge from `seller.tier`.
3. Listed price → effective cost, then one line per `AppliedVoucher` (label, amount,
   a marker when `contested`), shipping net, expected cashback when non-zero.
4. Exit: dealer, quote, colour multiplier when ≠ 100%, invoice penalty when non-zero,
   decay haircut and amount, exit price, quote age (flag when stale).
5. **Net profit** and **ROI**; when the cost has contested vouchers, a second line with
   `net_profit_if_contested_fail`.
6. Risk flags, one short Vietnamese phrase each; VAT status; delivery estimate; stock
   count if known; observation age and extraction method when it is `CssSelector`.
7. Footer: detection time in ICT (`arb_core::ICT`, `HH:MM:SS dd/MM`), pipeline latency.

Use the `Display` impls of `VndAmount` / `Bps` / `SignedBps`. `silent = true` for
`Normal`. Buttons: one URL button to `opportunity.deep_link()`. Keep the text under
4096 characters; if voucher lines would exceed it, truncate them with a count.

### Dispatcher

`pub fn spawn(config: NotifyConfig, alerts: mpsc::Receiver<AlertEvent>, control: Arc<dyn ControlPlane>, audit: Arc<dyn AlertAudit>, shutdown: CancellationToken) -> Result<JoinHandle<Result<(), ArbError>>, ArbError>`
where `AlertAudit` is a small trait you define in `arb-notify` (record / mark) and
implement for `Store` in `arb-cli` — `arb-notify` must not depend on `arb-storage`.
`NotifyConfig { bot_token, alert_chat_id, admin_user_ids }` with redacting `Debug`.

- `teloxide` bot with the `Throttle` adaptor. Send path: audit `pending` → send →
  audit `sent` (message id) or `failed`. On `RetryAfter`, wait and retry; on network
  errors retry with capped backoff up to 5 attempts; never drop an alert silently —
  a final failure is audited and logged at `error`.
- Abstract the send behind a small trait so tests use a fake sink (no network in tests).
- Commands (`BotCommands` derive), replies in Vietnamese, all MarkdownV2-escaped:
  - `/watch <sku>` and `/unwatch <sku>` — admin.
  - `/set_exit <sku> <vnd_price>` — admin. Accept `28500000`, `28.500.000`, `28,5tr`,
    `28.5m`; parse with integer math only; echo the stored quote and the previous one.
  - `/vouchers` — no argument: list active rules (id, label, layer, discount, contested,
    time left). With an attached `.toml` document or a TOML code block in the message:
    parse as `VoucherBook`, call `replace_vouchers`, reply with the active-rule count or
    the validation error verbatim. Admin for the mutating form.
  - `/top_spreads [n]` — default 5, max 20; compact one-line-per-spread table.
  - `/health` — mode, active window, per-connector circuit state and error rate, stale
    quotes, alerts emitted/suppressed, events dropped, polls shed.
- Authorization: mutating commands require `from.id ∈ admin_user_ids`; everyone else gets
  a refusal and the attempt is logged. Commands are accepted only from the alert chat or
  a private chat with an admin.
- Inline callback buttons on alerts: "Mute SKU 1h" (suppresses sends for that SKU in the
  dispatcher, bounded map, expires by the injected clock) and "Top spreads".

Tests: snapshot tests for `render_alert` over fixtures covering every `AlertKind`, every
`RiskFlag`, contested and uncontested, colour unknown, stale quote; a property test that
arbitrary titles/labels/dealer names produce text in which every reserved character
outside intended formatting is escaped; price-argument parser table; authorization
matrix; dispatcher with a fake sink: ordering preserved, `RetryAfter` honoured under
`tokio::time::pause()`, failed sends audited, shutdown drains the queue.

## Stage B — `arb-cli`

Binary `arb` with `clap` subcommands:

- `run --engine <path> --skus <path> --vouchers <path> [--dry-run]`
- `check-config …` — load and `validate()` all three files, print a summary, exit code.
- `replay …` — **seam for another agent**: declare `mod replay;` with
  `src/replay/mod.rs` containing `pub struct Args` (a `clap::Args` with no fields) and
  `pub async fn run(_: Args) -> anyhow::Result<()> { Ok(()) }`. Do nothing else there.

`run` wiring, in order:
1. `tracing-subscriber`: JSON to stdout, `ARB_LOG` env filter, ICT timestamps.
2. Load + validate configs (`toml`). Fail fast with the offending key in the message.
3. `Store::open`. Restore `latest_operator_quotes` and, if newer than the file,
   `latest_voucher_book`.
4. `arb_connectors::build_connectors(&config, &Secrets::from_env(), clock)`.
5. `arb_engine::spawn(EngineDeps { … }, shutdown.child_token())`.
6. Wrap `handle.control` in a `PersistingControlPlane` decorator (your type, in
   `arb-cli`) that forwards to the engine and, on success, appends `/set_exit` and
   `/vouchers` changes to the store.
7. `spawn_writer(store, handle.events, …)`; `arb_notify::spawn(…, handle.alerts, control, …)`.
8. Voucher file watcher (`notify`, debounced 500 ms): reload → `replace_vouchers`; on
   error keep the old book and send one operator message.
9. `--dry-run`: replace the Telegram sink with a logging sink; everything else is real.

Supervision and shutdown:
- `tokio::signal` for SIGINT and SIGTERM → cancel the root `CancellationToken`.
- Drain order: engine task → notify (alerts drained) → storage writer (flush +
  checkpoint) → `Store::close`. Overall deadline 15 s; a second signal exits immediately.
- Every task's `JoinHandle` is awaited. A task that ends with `Err` or panics while not
  shutting down: log, send one operator message, cancel everything, exit non-zero (a
  process supervisor such as systemd restarts it — do not build an in-process restart
  loop for stateful tasks).
- Startup message to the alert chat: version, enabled sources, watched SKU count, active
  voucher count, stale-quote count.

Also ship `deploy/arb.service` (systemd unit with `Restart=on-failure`,
`EnvironmentFile=`) and `.env.example` listing every variable from
`docs/02-architecture.md` §6 with empty values.

Tests: config loading against `config/*.toml`; `PersistingControlPlane` persists only on
success; an end-to-end test with fake connectors (in-process `PriceConnector` impls), a
fake Telegram sink and a temp DB that produces one alert, then shuts down cleanly on
cancellation with all tasks joined inside the deadline.

## Deliverable

- All three crates passing fmt, `clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`.
- `README.md` in each crate; the operator runbook (first run, campaign-day checklist:
  refresh quotes, load vouchers, turn on `record_observations`, check `/health`) in
  `crates/arb-cli/README.md`.
- Final message: unfinished items, assumptions, change requests.
