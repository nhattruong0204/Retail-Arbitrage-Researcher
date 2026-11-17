//! `arb-core` — the frozen contract every other crate compiles against.
//!
//! Ownership: Fable (orchestrator). Sub-agents MUST NOT edit this crate; if a contract
//! is wrong or missing, stop and file a change request (see `docs/03-orchestration.md`).
//!
//! Layout:
//! - [`money`] — `VndAmount`, `Bps`, `SignedBps`, explicit rounding. Integer-only.
//! - [`ids`] — validated slug identifiers and the `SourceId` enum.
//! - [`sku`] — `CanonicalSku`, colors, source mappings, `BuybackQuote`, `SkuCatalog`.
//! - [`listing`] — `RawMarketListing` and its attribute enums.
//! - [`voucher`] — `VoucherRule`, `VoucherBook`.
//! - [`opportunity`] — cost/exit breakdowns, `ArbitrageOpportunity`, events.
//! - [`config`] — `EngineConfig` (thresholds, schedule, decay, filter, dedup, sources).
//! - [`ports`] — `PriceConnector`, `ControlPlane`, `Clock` traits.
//! - [`error`] — `ArbError`.

pub mod config;
pub mod error;
pub mod ids;
pub mod listing;
pub mod money;
pub mod opportunity;
pub mod ports;
pub mod sku;
pub mod voucher;

pub use config::{
    CampaignDay, CashbackConfig, CashbackMode, ChannelConfig, DecayConfig, DecayTier, DedupConfig,
    EngineConfig, FilterConfig, FlashWindow, ScheduleConfig, SourceConfig, ThresholdMode,
    Thresholds,
};
pub use error::{ArbError, ParseStage};
pub use ids::{ColorId, SkuId, SourceId, SourceKind};
pub use listing::{
    Availability, DeliveryEstimate, ExtractionMethod, FetchOutcome, FetchRequest, FlashSaleInfo,
    ItemCondition, RawMarketListing, SellerInfo, SellerTier, VatInvoiceStatus,
};
pub use money::{Bps, Rounding, SignedBps, VndAmount};
pub use opportunity::{
    AlertEvent, AlertFingerprint, AlertKind, AlertPriority, AppliedVoucher, ArbitrageOpportunity,
    CostBreakdown, EngineEvent, ExitBreakdown, PriceTick, RejectReason, RejectedListing, RiskFlag,
    net_profit_and_roi,
};
pub use ports::{
    BoxFuture, CircuitState, Clock, ConnectorCapabilities, ConnectorHealth, ControlPlane,
    HealthReport, PollMode, PriceConnector, SystemClock,
};
pub use sku::{
    BuybackQuote, CanonicalSku, Category, City, ColorVariant, QuoteSource, SealedCondition,
    SkuCatalog, SourceMapping,
};
pub use voucher::{
    DiscountBasis, DiscountKind, PaymentMethod, VoucherBook, VoucherLayer, VoucherRule,
};

/// The only timezone business logic may reason in. `Asia/Ho_Chi_Minh` is UTC+7 with no DST.
/// Timestamps are stored as `DateTime<Utc>` and converted to `ICT` at the edges
/// (schedule evaluation, alert rendering).
pub const ICT: chrono_tz::Tz = chrono_tz::Asia::Ho_Chi_Minh;

/// Bumped on any breaking change to a serialized contract (config files, stored JSON).
pub const SCHEMA_VERSION: u32 = 1;
