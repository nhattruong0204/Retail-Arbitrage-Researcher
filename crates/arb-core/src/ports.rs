//! Ports: the trait seams between crates.
//!
//! Why boxed futures instead of `async fn` in traits: every port here is used as
//! `Arc<dyn Trait>` (a heterogeneous connector pool; a control plane injected into the
//! bot). Native `async fn` in traits is not dyn-compatible, and `#[async_trait]` expands
//! to exactly this signature anyway — so we write it out and take no macro dependency.
//! Implementors write `Box::pin(async move { ... })`. One allocation per network call
//! is noise next to the network call.

use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::ArbError;
use crate::ids::{SkuId, SourceId};
use crate::listing::{FetchOutcome, FetchRequest};
use crate::money::VndAmount;
use crate::opportunity::ArbitrageOpportunity;
use crate::sku::{BuybackQuote, CanonicalSku};
use crate::voucher::VoucherBook;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Injected time source. Production uses [`SystemClock`]; replay and tests inject a
/// controllable clock. No crate other than `arb-cli` may call `Utc::now()` directly.
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorCapabilities {
    pub supports_search: bool,
    /// Sends `If-None-Match` / `If-Modified-Since` and can return `NotModified`.
    pub supports_conditional_get: bool,
    pub reports_commission: bool,
    pub reports_stock_count: bool,
    pub issues_affiliate_links: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CircuitState {
    Closed,
    HalfOpen,
    Open,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorHealth {
    pub source: SourceId,
    pub circuit: CircuitState,
    pub consecutive_failures: u32,
    pub requests_total: u64,
    pub errors_total: u64,
    pub not_modified_total: u64,
    pub last_success_at: Option<DateTime<Utc>>,
    /// `ArbError::kind()` of the most recent failure.
    pub last_error_kind: Option<String>,
}

/// One data source. Implementations own their HTTP client, rate limiter, retry policy
/// and circuit breaker; callers may invoke `fetch` concurrently and freely.
///
/// Contract:
/// - `fetch` never panics and never blocks the executor.
/// - It waits for the rate limiter internally; it returns `RateLimited` only if the
///   wait would exceed the source timeout.
/// - Every returned listing passes `RawMarketListing::check_invariants` and has
///   `source == self.source()`; `sku_hint` is set iff the request was `Mapped`.
/// - Cancel-safe: dropping the future mid-flight leaks nothing and corrupts no state.
pub trait PriceConnector: Send + Sync + 'static {
    fn source(&self) -> SourceId;

    fn capabilities(&self) -> ConnectorCapabilities;

    fn fetch<'a>(
        &'a self,
        request: &'a FetchRequest,
    ) -> BoxFuture<'a, Result<FetchOutcome, ArbError>>;

    /// Cheap, synchronous snapshot of counters. Must not perform I/O.
    fn health(&self) -> ConnectorHealth;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PollMode {
    Baseline,
    Burst,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthReport {
    pub started_at: DateTime<Utc>,
    pub now: DateTime<Utc>,
    pub mode: PollMode,
    /// Label of the active flash window, if any.
    pub active_window: Option<String>,
    pub connectors: Vec<ConnectorHealth>,
    pub watched_skus: u32,
    pub active_vouchers: u32,
    pub stale_buyback_quotes: u32,
    pub alerts_emitted: u64,
    pub alerts_suppressed: u64,
    /// Events dropped from the lossy event bus since start.
    pub events_dropped: u64,
    /// Scheduled polls skipped because the rate limit left no budget.
    pub polls_shed: u64,
}

/// Live operator controls, implemented by the engine and consumed by the Telegram bot.
/// All mutations take effect for the next evaluation without a restart; persistence of
/// overrides across restarts is the binary's job (it decorates this trait).
pub trait ControlPlane: Send + Sync + 'static {
    /// Enable polling for a SKU that exists in the catalog.
    fn watch<'a>(&'a self, sku_id: &'a SkuId) -> BoxFuture<'a, Result<CanonicalSku, ArbError>>;

    fn unwatch<'a>(&'a self, sku_id: &'a SkuId) -> BoxFuture<'a, Result<(), ArbError>>;

    /// Install an operator quote that supersedes config quotes for the SKU.
    /// Returns the quote as stored (`source = Operator`, `quoted_at = now`).
    fn set_exit<'a>(
        &'a self,
        sku_id: &'a SkuId,
        price: VndAmount,
    ) -> BoxFuture<'a, Result<BuybackQuote, ArbError>>;

    /// Atomically swap the whole voucher book. Validates first; on error the old book
    /// stays in force. Returns the number of rules active right now.
    fn replace_vouchers(&self, book: VoucherBook) -> BoxFuture<'_, Result<u32, ArbError>>;

    fn vouchers(&self) -> BoxFuture<'_, Result<VoucherBook, ArbError>>;

    /// Best current spread per (sku, colour, source), highest net profit first,
    /// including sub-threshold `Watch` spreads.
    fn top_spreads(
        &self,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<ArbitrageOpportunity>, ArbError>>;

    fn health(&self) -> BoxFuture<'_, Result<HealthReport, ArbError>>;
}
