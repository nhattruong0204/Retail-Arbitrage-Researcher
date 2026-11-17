# Prompt S-1 — `arb-connectors` (Claude Sonnet 5.5)

You are implementing one crate in an existing Rust workspace: `crates/arb-connectors`.
It turns public retailer product pages and official marketplace affiliate APIs into
`arb_core::RawMarketListing` values. Other agents are building sibling crates in
parallel against the same frozen contracts.

## Read first

1. `crates/arb-core/src/**` — especially `listing.rs` (`RawMarketListing`,
   `check_invariants`, `FetchRequest`, `FetchOutcome`), `ports.rs` (`PriceConnector`
   contract in the trait docs, `ConnectorHealth`, `Clock`), `error.rs` (`ArbError`,
   `ParseStage`, `is_retryable`, `trips_circuit`), `config.rs` (`SourceConfig`),
   `ids.rs` (`SourceId::rate_limit_domain`).
2. `docs/02-architecture.md` §5.1 — your pinned facade.
3. `docs/01-research-brief.md` §1 (HTTP/rate-limit/HTML decisions), §4 (failure modes),
   §5 (marketplace API notes, including what is marked VERIFY).

## Compliance rules (non-negotiable; violations fail review outright)

- Only public product pages and the **official, documented** Shopee Affiliate Open API
  and Lazada Open Platform API. No private/mobile/internal endpoints.
- Send the configured `EngineConfig::user_agent` unchanged. No browser impersonation, no
  spoofed headers, no cookies or sessions, no proxy rotation, no CAPTCHA handling, no
  JavaScript execution, no cart/checkout requests.
- A 403 or 429 means back off. Never retry them; never vary the request to get through.
- Every outbound request passes the per-domain rate limiter. No exceptions, including
  tests against `wiremock` (use a fast limiter config, not a bypass).

## Hard rules

- Touch only `crates/arb-connectors/**`. Contract problems → `CHANGE_REQUEST.md`
  (`docs/03-orchestration.md` §3).
- Dependencies only from root `[workspace.dependencies]` via `{ workspace = true }`:
  `reqwest`, `governor`, `backon`, `scraper`, `serde`, `serde_json`, `url`, `sha2`,
  `hmac`, `hex`, `tokio`, `chrono`, `tracing`; dev: `wiremock`, `tokio` (`test-util`).
- No `unwrap`/`expect`/`panic!` outside tests. Nothing from the network is trusted:
  no indexing with `[]`, no unchecked arithmetic, no `as` narrowing.
- Prices are parsed to `VndAmount` as integers. Accept `"32990000"`, `32990000`,
  `"32.990.000"`, `"32,990,000"`, `"32990000.00"`, `32990000.0`; reject anything with a
  non-zero fractional part or that is not a plain number. If a JSON number arrives as a
  float, convert through its string form — do not do float arithmetic (clippy denies it).
- Timestamps come from the injected `Arc<dyn Clock>`, never `Utc::now()`.
- `scraper::Html` is `!Send`: parse and extract inside a synchronous function and return
  owned data; for bodies over 300 KB do it in `tokio::task::spawn_blocking`.
- Cap response bodies at 5 MB (check `Content-Length`, and enforce while reading).

## Public API (binding, verbatim)

`Secrets`, `Secrets::from_env`, `build_connectors` exactly as in
`docs/02-architecture.md` §5.1. `Debug` for `Secrets` is hand-written and redacts values.

## What to build

### 1. `http` — shared client middleware

`SourceHttp`, built from a `SourceConfig` + user agent + clock:
- one `reqwest::Client` per source: rustls, HTTP/2 via ALPN, gzip/brotli, connect timeout
  3 s, total timeout `timeout_ms`, `pool_max_idle_per_host` small, redirects limited to 3
  and same-registrable-domain only;
- `governor` limiter keyed by `rate_limit_domain()` (`requests_per_minute`, `burst`).
  Connectors sharing a domain must share one limiter — hold limiters in a registry keyed
  by domain inside `build_connectors`. Wait with `until_ready()`; if the wait would exceed
  `timeout_ms`, return `ArbError::RateLimited`;
- concurrency semaphore (`max_concurrency`);
- conditional GET: bounded in-memory map `url → (ETag, Last-Modified)`; send
  `If-None-Match` / `If-Modified-Since`; 304 → `FetchOutcome::NotModified`;
- retry with `backon`: exponential, full jitter, max 2 retries, total budget ≤
  `timeout_ms`, only when `ArbError::is_retryable()`; honour `Retry-After` (seconds or
  HTTP-date) and surface it in `HttpStatus::retry_after_secs`;
- circuit breaker: `breaker_failure_threshold` consecutive `trips_circuit()` errors →
  `Open` for `breaker_open_secs` → `HalfOpen` (one probe) → `Closed`. While open, return
  `CircuitOpen { retry_at }` without touching the network. 429 opens it immediately;
- counters for `ConnectorHealth` in atomics; `health()` does no I/O;
- map every `reqwest` error to `Transport` / `Timeout` / `HttpStatus` at this boundary.

### 2. `jsonld` — generic Schema.org extractor (pure functions)

Input: HTML text. Output: zero or more product offers.
- Find every `<script type="application/ld+json">`; tolerate HTML comments/CDATA
  wrappers, leading BOM, and trailing commas only if `serde_json` accepts the block as-is
  (do not write a lenient JSON parser — an unparsable block is skipped and counted).
- Walk: top-level object, top-level array, and `@graph`. `@type` may be a string or an
  array; match `Product` (and `ProductGroup` with `hasVariant`).
- `offers` may be an `Offer`, an array of `Offer`, or an `AggregateOffer`. For
  `AggregateOffer` use `lowPrice` **only** when `offerCount` is absent or 1, or when
  `lowPrice == highPrice`; otherwise the page covers several variants and the price is
  ambiguous → return a `Parse { stage: JsonLd }` error naming the ambiguity so the
  adapter can fall back.
- Require `priceCurrency == "VND"` when present.
- Map `availability` (accept both `https://schema.org/InStock` and bare `InStock`) to
  `Availability`; `itemCondition` to `ItemCondition`; absent → `Unknown`.
- Extract `name`, `sku`/`mpn`/`productID`, `url`, `color` when present.

### 3. Retail adapters — Thế Giới Di Động, CellphoneS, ShopDunk

One `RetailConnector` generic over a small `RetailAdapter` description per site:
`SourceId`, allowed host(s), ordered extraction strategies (`JsonLd` → optional catalog
JSON embedded in the page → CSS selectors), and CSS selectors for price/title/stock as
the last resort. Retailers support `FetchRequest::Mapped` only (`Search` →
`ArbError::Unsupported`). The mapping URL host must equal an allowed host for the source.
Set `seller.tier = AuthorizedReseller`, `sku_hint` and `color_hint` from the request,
`extraction` to the strategy that produced the price, `vat_invoice = Unknown` (the engine
applies the source default), `observed_at` from the clock, `source_updated_at` from
`Last-Modified` when present.

**You do not have live access to these sites and must not guess that you do.** Build the
adapters against fixtures: create `tests/fixtures/<source>/` with *synthetic* HTML you
write yourself that exercises the extractor (clearly marked synthetic in a README there),
and make the CSS selectors and any embedded-JSON key paths **data in the adapter
definition**, with a doc comment saying they must be confirmed against a real captured
page before production use. List every such unverified selector in your final message.
FPT Shop and Hoàng Hà get adapter definitions with JSON-LD only and no CSS fallback.

### 4. Marketplace clients — Shopee Affiliate (GraphQL), Lazada (REST)

Each supports `Mapped` and `Search`, sets `extraction = AffiliateApi`,
`seller.tier = OfficialMall` only when the API says so (official-shop / mall flag),
otherwise `ThirdParty`; fills `commission`, `affiliate_url`, `shop_id`, rating
(`rating_milli`, parsed from the decimal string without float math), `stock_count` and
`flash_sale` when the API provides them. Response structs use `#[serde(default)]` and
ignore unknown fields.

- **Shopee**: POST GraphQL to `{base_url or https://open-api.affiliate.shopee.vn}/graphql`.
  Signing lives in one pure function `sign_shopee(app_id, secret, timestamp, body) ->
  String` producing the `Authorization` header value. Implement the scheme described in
  `docs/01-research-brief.md` §5 and mark it `// VERIFY against official docs` — the unit
  test must use a test vector, and you must state in your final message whether that
  vector came from official documentation or was self-generated.
- **Lazada**: GET `{base_url or https://api.lazada.vn/rest}{path}` with `app_key`,
  `timestamp` (ms), `sign_method=sha256`, optional `access_token`, and `sign` =
  upper-case hex HMAC-SHA256(secret, path + concat(sorted `key`+`value`)). Pure function
  `sign_lazada(secret, path, params) -> String`, same test-vector requirement.
- API-level errors (HTTP 200 with an error body) map to `UpstreamApi` or `Auth`.
- Timestamps for signing come from the injected clock.

### 5. `build_connectors`

One connector per enabled `[[source]]`; shared limiter per domain; enabled marketplace
source with missing credentials → `ArbError::Config` naming the missing variable.

## Tests (required, all offline)

`wiremock` per connector, with `SourceConfig::base_url` pointed at the mock:
success → correct `RawMarketListing` (assert every field); 304 on second fetch with the
right conditional headers; 429 → no retry, breaker opens, next call is `CircuitOpen`
without a request; 503 then 200 → one retry, success; 503 ×3 → error, breaker counts;
timeout; truncated HTML; page with no JSON-LD → fallback strategy used and
`extraction` reflects it; `OutOfStock`; `AggregateOffer` ambiguous; price formats table;
body over the size cap; redirect to a foreign host refused.
Rate limiter: N calls under `tokio::time::pause()` take at least the expected virtual
time; two connectors on one domain share the budget.
Signing: test vectors. `Secrets` `Debug` output contains no secret.
Extractor unit tests over the fixture corpus, including malformed blocks.

## Deliverable

- The crate, passing fmt, `clippy --all-targets -- -D warnings`, `cargo test -p arb-connectors`.
- `crates/arb-connectors/README.md`: how to capture a real page into a fixture and
  confirm an adapter; the list of unverified selectors/endpoints/field names.
- Final message: unfinished items, assumptions, VERIFY items, change requests.
