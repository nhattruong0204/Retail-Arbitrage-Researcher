# Phase 3 — Multi-Agent Orchestration Plan

Roles: **Fable** = orchestrator/reviewer (owns `arb-core`, configs, docs, prompts).
**Opus** = O-1, O-2. **Sonnet** = S-1, S-2.

## 1. Dependency DAG

```
Wave 0  Fable   arb-core + configs + skeleton crates            [this commit]
          │      gate G0
          ├───────────────┬────────────────────┐
Wave 1  O-1 arb-engine   S-1 arb-connectors   S-2 (stage A) arb-storage, arb-notify
          │               │                    │
          └──── gate G1 ──┴────────────────────┘
Wave 2  S-2 (stage B) arb-cli wiring                             gate G2
Wave 3  O-2 audit + replay + stress                              gate G3
```

S-2 is one prompt with two stages. Stage A depends only on `arb-core` and the pinned
facades, so it runs alongside Wave 1. Stage B (the binary) needs the real `arb-engine` and
`arb-connectors` and starts after G1.

## 2. File ownership (zero-overlap rule)

| Path | Owner | Others may |
|---|---|---|
| `crates/arb-core/**`, `config/**`, `docs/**`, `prompts/**`, root `Cargo.toml`, `clippy.toml`, `rust-toolchain.toml` | Fable | read only |
| `crates/arb-engine/**` | O-1 | read only |
| `crates/arb-connectors/**` | S-1 | read only |
| `crates/arb-storage/**`, `crates/arb-notify/**` | S-2 | read only |
| `crates/arb-cli/Cargo.toml`, `crates/arb-cli/src/**` except `src/replay/**` | S-2 | read only |
| `crates/arb-cli/src/replay/**`, `crates/arb-cli/tests/**`, `AUDIT.md` | O-2 | read only |
| `Cargo.lock` | whoever's change required it; committed with that change | — |

Each agent works on its own branch (`wave1/o1-engine`, `wave1/s1-connectors`,
`wave1/s2-storage-notify`, `wave2/s2-cli`, `wave3/o2-audit`). Because paths are disjoint,
merges are fast-forward or trivially clean. A diff touching a path the agent does not own
is rejected without review.

S-2 leaves exactly one seam for O-2 in `arb-cli/src/main.rs`: a `Replay(replay::Args)`
subcommand that calls `replay::run(args).await`, with `mod replay;` pointing at
`src/replay/mod.rs` containing a stub `run` that returns `Ok(())`. O-2 replaces the
contents of `src/replay/` only.

### Adding a dependency

Agents may use any crate already in `[workspace.dependencies]` by adding
`name = { workspace = true }` to their own crate. A crate that is *not* listed is a change
request (below) — do not add a version to a member `Cargo.toml`.

## 3. Change requests to `arb-core`

Contracts are frozen, not infallible. If an agent finds a contract that is wrong, missing
or ambiguous, it must **stop that line of work** and write
`CHANGE_REQUEST.md` at the root of its own crate with:

1. the type/trait and file;
2. what is needed and why the current contract cannot express it;
3. the smallest proposed diff;
4. what it did in the meantime (usually: a private adapter type inside its own crate).

It must not edit `arb-core`, shadow a core type with a look-alike, or widen a type with
`String`/`serde_json::Value` to get unblocked. Fable batches requests at each gate, applies
accepted ones in a single `arb-core` commit, bumps `SCHEMA_VERSION` if serialized shapes
changed, and re-issues affected prompts.

## 4. Gates

Every gate runs from a clean checkout:

```
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

| Gate | Additional criteria |
|---|---|
| **G0** | `cargo generate-lockfile` succeeds and `Cargo.lock` is committed. `arb-core` unit tests and `tests/config_files.rs` pass against the real `serde`/`toml`/`chrono`. Any version floor that fails to resolve or compile is corrected in the root manifest. Shopee/Lazada signing schemes and quotas marked VERIFY in the research brief are confirmed against official docs and the brief is updated. |
| **G1** | Engine: proptest suites pass with `PROPTEST_CASES=2000`; a 24-simulated-hour scheduler test under `tokio::time::pause()` shows no rate-limit violation and correct mode transitions. Connectors: every adapter has `wiremock` tests for success, 304, 429, 503, truncated HTML, missing JSON-LD, `OutOfStock`; signing functions match the official test vectors. Storage/notify: migrations apply to an empty DB; `render_alert` snapshot tests pass; MarkdownV2 escaping property test passes. No `CHANGE_REQUEST.md` left unresolved. |
| **G2** | `arb-cli run --dry-run` starts with the example configs against `wiremock`-backed sources, emits at least one alert to a fake Telegram sink, and exits 0 on SIGTERM within 10 s with all tasks joined. |
| **G3** | Replay of the bundled fixture session is byte-identical across two runs. Stress test: 10× nominal listing rate for 60 simulated minutes with no alert loss, bounded memory, and `events_dropped` accounted for. `AUDIT.md` lists every finding with severity and a fix or a justification. |

## 5. Review protocol (Fable)

Per submission, in this order. Any **blocker** sends the work back with the exact item.

**A. Mechanical (blocker)**
- Diff stays inside owned paths. Gate commands pass locally.
- `rg -n "unwrap\(|expect\(|panic!|unreachable!|todo!|unimplemented!" crates/<crate>/src`
  returns only `#[cfg(test)]` code. `unreachable!` needs a proof comment.
- `rg -n "\bf(32|64)\b" crates/{arb-engine,arb-storage}/src` is empty; any hit elsewhere
  has a local `#[allow(clippy::float_arithmetic)]` with a reason.
- `rg -n "Utc::now\(|Instant::now\(|SystemTime::now\(" crates/` hits only `arb-cli` and
  `arb-core::SystemClock` (engine and connectors take the injected `Clock`;
  `tokio::time::Instant` is allowed for intervals).
- `rg -n "as (i64|u64|u32|i32|usize)\b"` — every hit is a provably lossless widening or is
  replaced by `try_from`.

**B. Mathematical correctness (blocker)**
- Recompute three opportunities by hand from fixture inputs (one capped-percent voucher,
  one `after_prior_layers` stack, one with no invoice) and compare every field.
- Confirm all cost/exit numbers flow through `CostBreakdown::build` /
  `ExitBreakdown::build` — no second implementation of the identities.
- Property tests exist and are meaningful for: adding a voucher never raises `C_eff`;
  raising the listed price never raises net profit; a larger decay haircut never raises
  `P_exit`; `net_profit_if_contested_fail ≤ net_profit`; stacking is order-independent
  with respect to *input* order of rules; no input makes `ingest` panic.
- Voucher validity is judged at `observed_at`; windows are half-open.

**C. Task starvation and blocking**
- No blocking call on the async executor (`std::fs`, `std::thread::sleep`, sync DNS,
  large `scraper` parses outside `spawn_blocking`).
- No lock guard alive across `.await` (check every `Mutex`/`RwLock`; prefer none).
- Long loops over collections yield or are bounded; the pipeline task cannot be held up
  by the event bus (must be `try_send`).
- `select!` arms are cancel-safe, with `biased;` where shutdown must win.
- Dispatcher fairness: a permanently failing SKU cannot monopolize a domain's budget
  (backoff per job, not just per source).

**D. Memory growth**
- Every map/cache/queue has a bound and an eviction rule: medians (`median_window`),
  dedup (`max_entries`), best-spread table (≤ SKUs × colours × sources), job heaps
  (≤ mappings), channels (configured capacities), ETag cache (≤ mappings).
- No `tokio::spawn` without a retained `JoinHandle` or a `JoinSet`; no task that outlives
  shutdown.
- Response bodies are size-capped before buffering (reject > 5 MB).
- `Arc` cycles: none (check any `Arc<Self>` captured by a task the struct also owns).

**E. Panic safety and failure containment**
- Indexing: `get()` or iterator, not `[]`, on anything derived from input.
- Integer ops on external values are checked.
- A connector error never stops a dispatcher; a pipeline error on one listing is logged
  and counted and the next listing proceeds.
- Supervisor restarts with backoff and reports; a restart loop is capped.

**F. Compliance (blocker)**
- No header spoofing, no cookie/session reuse, no proxy rotation, no CAPTCHA handling, no
  checkout or cart automation, no undocumented/private marketplace endpoints.
- The configured `User-Agent` is sent as-is on retailer requests.
- 403/429 lead to backing off.

**G. Tests are real**
- Tests assert values, not just "is_ok". Fixtures are checked in, not fetched.
- No test sleeps on wall time; use `tokio::time::pause()` and the injected clock.

Findings are recorded per submission as `blocker / major / minor` with file:line. The
re-submission must answer each finding.

## 6. Risk register

| Risk | Mitigation |
|---|---|
| Version floors do not resolve at G0 | Fable fixes the root manifest before Wave 1 starts; agents never pin versions. |
| Retailer markup differs from assumptions | S-1 adapters are thin config over a generic extractor; fixtures are captured manually by the operator and dropped into `tests/fixtures/`. |
| Affiliate API shape differs from the brief | Clients are isolated behind `PriceConnector`; response structs use `#[serde(default)]` and tolerate unknown fields; VERIFY items close at G0. |
| Facade mismatch between O-1 and S-2 | Facades are pinned verbatim in both prompts; G1 compiles S-2 stage A against O-1's real crate. |
| Agents "fix" core types locally | Mechanical review step A + change-request protocol. |
