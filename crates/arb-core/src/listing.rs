//! What a connector returns: one observed offer, normalized but not yet judged.
//! Connectors report facts and `Unknown`s; they never guess, filter or score.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::ArbError;
use crate::ids::{ColorId, SkuId, SourceId};
use crate::money::{Bps, VndAmount};
use crate::sku::SourceMapping;

/// Maps 1:1 onto `schema.org/ItemAvailability` where the source provides it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    InStock,
    LimitedAvailability,
    OutOfStock,
    PreOrder,
    Discontinued,
    Unknown,
}

impl Availability {
    /// Can an order be placed for delivery from stock right now?
    pub const fn is_purchasable(self) -> bool {
        matches!(self, Self::InStock | Self::LimitedAvailability)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemCondition {
    New,
    Used,
    Refurbished,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SellerTier {
    /// Shopee Mall / LazMall official store.
    OfficialMall,
    /// Brand-authorized reseller (the retail chains; AAR/APR stores).
    AuthorizedReseller,
    /// Ordinary third-party marketplace seller.
    ThirdParty,
    Unknown,
}

impl SellerTier {
    /// Only these tiers reliably issue VAT e-invoices and genuine VN/A stock.
    pub const fn is_authorized(self) -> bool {
        matches!(self, Self::OfficialMall | Self::AuthorizedReseller)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VatInvoiceStatus {
    /// Issued by default or on checkout.
    Available,
    /// Must be requested out-of-band; treat as uncertain.
    OnRequest,
    Unavailable,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionMethod {
    JsonLd,
    CatalogJson,
    /// Fallback DOM scraping: lowest trust, flagged in alerts.
    CssSelector,
    AffiliateApi,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SellerInfo {
    pub name: String,
    pub tier: SellerTier,
    /// Rating × 1000 (4.9★ = 4900). Integer so the whole listing stays `Eq`.
    pub rating_milli: Option<u16>,
}

/// Inclusive delivery window in calendar days from order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryEstimate {
    pub min_days: u8,
    pub max_days: u8,
}

impl DeliveryEstimate {
    pub fn validate(&self) -> Result<(), ArbError> {
        if self.min_days > self.max_days {
            return Err(ArbError::config("delivery_estimate", "min_days > max_days"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlashSaleInfo {
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    /// Flash price if the API exposes it ahead of the window.
    pub price: Option<VndAmount>,
    pub stock: Option<u32>,
}

/// One observed offer for (probably) one SKU on one source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawMarketListing {
    pub source: SourceId,
    /// Stable per-source id. Retailers without one use the canonical URL path.
    pub platform_item_id: String,
    pub shop_id: Option<String>,
    /// Canonical product URL (no tracking parameters).
    pub url: String,
    /// Affiliate tracking link, if the source issued one.
    pub affiliate_url: Option<String>,
    /// Title exactly as published. Normalization is the engine's job.
    pub title: String,

    /// Set iff this listing was fetched through a curated `SourceMapping`.
    pub sku_hint: Option<SkuId>,
    pub color_hint: Option<ColorId>,

    /// The price a buyer is charged before any voucher. For `AggregateOffer` this is
    /// `lowPrice`; connectors must not substitute a crossed-out or "from" price.
    pub listed_price: VndAmount,
    /// Crossed-out reference price, informational only.
    pub original_price: Option<VndAmount>,

    pub availability: Availability,
    pub stock_count: Option<u32>,
    pub condition: ItemCondition,
    pub seller: SellerInfo,
    pub vat_invoice: VatInvoiceStatus,

    /// Affiliate commission rate reported by the platform for this item.
    pub commission: Option<Bps>,
    /// `None` = unknown (engine applies the source default); `Some(0)` = free.
    pub shipping_fee: Option<VndAmount>,
    pub delivery: Option<DeliveryEstimate>,
    pub flash_sale: Option<FlashSaleInfo>,

    pub extraction: ExtractionMethod,
    /// When our process received the response (from the injected `Clock`).
    pub observed_at: DateTime<Utc>,
    /// Source-side freshness (`Last-Modified`, API `updateTime`), if known.
    pub source_updated_at: Option<DateTime<Utc>>,
}

impl RawMarketListing {
    /// Structural validity only — a connector bug guard, not a business filter.
    /// Connectors call this before returning; a failure is a `Parse` error upstream.
    pub fn check_invariants(&self) -> Result<(), ArbError> {
        let fail = |message: &str| ArbError::Parse {
            source_id: self.source,
            stage: crate::error::ParseStage::Price,
            message: message.to_owned(),
        };
        if !self.listed_price.is_positive() {
            return Err(fail("listed_price must be > 0"));
        }
        if self.platform_item_id.is_empty() || self.title.trim().is_empty() {
            return Err(fail("platform_item_id and title must be non-empty"));
        }
        if !self.url.starts_with("https://") {
            return Err(fail("url must be https"));
        }
        if self.shipping_fee.is_some_and(VndAmount::is_negative) {
            return Err(fail("shipping_fee must be >= 0"));
        }
        if self.delivery.is_some_and(|d| d.min_days > d.max_days) {
            return Err(fail("delivery min_days > max_days"));
        }
        Ok(())
    }
}

/// One unit of work handed to a connector by the scheduler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchRequest {
    /// Fetch exactly the mapped item/page. `mapping.source` equals the connector's source.
    Mapped {
        sku_id: SkuId,
        mapping: SourceMapping,
    },
    /// Keyword search (marketplaces only). Results are noisy by definition.
    Search {
        sku_id: SkuId,
        query: String,
        limit: u16,
    },
}

impl FetchRequest {
    pub fn sku_id(&self) -> &SkuId {
        match self {
            Self::Mapped { sku_id, .. } | Self::Search { sku_id, .. } => sku_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchOutcome {
    /// Zero listings is valid (delisted, empty search).
    Listings(Vec<RawMarketListing>),
    /// Conditional GET returned 304: the previous observation still stands.
    NotModified,
}
