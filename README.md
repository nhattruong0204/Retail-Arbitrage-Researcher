# Retail Arbitrage Researcher

Real-time price monitor and Telegram alert engine for Vietnamese flash-sale events
(11/11, 12/12, payday and double-day campaigns), in Rust. It watches authorized retail
chains and official marketplace stores for a curated list of sealed consumer-electronics
SKUs, computes the **net** spread against sealed-buyback exit prices, and alerts when the
spread clears a threshold.

It informs; it does not buy. Data comes only from public product pages (rate-limited,
honestly identified) and official affiliate/open APIs. No checkout automation, no
anti-bot evasion.

## Status

| Wave | What | State |
|---|---|---|
| 0 | Workspace, `arb-core` contracts, config schemas, plan, sub-agent prompts | **done** (this commit) |
| 1 | `arb-engine` (O-1), `arb-connectors` (S-1), `arb-storage` + `arb-notify` (S-2 stage A) | not started |
| 2 | `arb-cli` wiring (S-2 stage B) | not started |
| 3 | Audit, replay simulator, stress suite (O-2) | not started |

**Wave 0 caveat:** this was authored in a sandbox without crates.io access. `arb-core`
was type-checked, linted and unit-tested (22 tests) against local stand-ins for
`serde`/`thiserror`/`chrono`, which proves the logic and types but not the serde derives
or the dependency versions. Gate G0 closes that gap:

```
cargo generate-lockfile          # then commit Cargo.lock
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace           # includes arb-core/tests/config_files.rs
```

## Read in this order

1. [`docs/01-research-brief.md`](docs/01-research-brief.md) — start with §0, the list of
   assumptions in the original brief that do not survive contact with reality.
2. [`docs/02-architecture.md`](docs/02-architecture.md) — data flow, contracts index,
   pinned cross-crate facades.
3. [`docs/03-orchestration.md`](docs/03-orchestration.md) — waves, ownership, gates,
   review protocol.
4. [`prompts/`](prompts) — the four self-contained implementation prompts.
5. [`crates/arb-core`](crates/arb-core/src) — the contracts themselves.

## Configuration

| File | Type | Purpose |
|---|---|---|
| `config/engine.toml` | `EngineConfig` | thresholds, schedule, decay, filters, dedup, sources |
| `config/skus.toml` | `SkuCatalog` | watchlist, per-source mappings, buyback quotes |
| `config/vouchers_11_11.toml` | `VoucherBook` | campaign vouchers (hot-reloadable) |

All prices, URLs and ids in the example configs are placeholders that show the schema.
