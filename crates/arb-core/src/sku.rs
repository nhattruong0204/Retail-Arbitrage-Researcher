//! Canonical SKUs, their per-source mappings, and buyback (exit) quotes.
//! `SkuCatalog` is the deserialized form of `config/skus.toml`.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::ArbError;
use crate::ids::{ColorId, SkuId, SourceId};
use crate::money::{Bps, VndAmount};

fn default_true() -> bool {
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Phone,
    Laptop,
    Tablet,
    Audio,
    Wearable,
    Other,
}

/// A colour of a SKU and how liquid it is on the buyback market.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColorVariant {
    pub id: ColorId,
    pub display: String,
    /// Lower-cased strings that identify this colour in listing titles
    /// (`"titan tự nhiên"`, `"natural titanium"`). Matched after normalization.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Multiplier on the base buyback floor. `10000` = pays the full floor;
    /// `9700` = dealers pay 3% less for this colour. Must be `<= 10000`.
    pub liquidity_bps: Bps,
}

/// Curated `(SKU -> platform item)` link. Stage 1 of entity resolution: a listing
/// fetched through a mapping is trusted to be this SKU and skips fuzzy matching
/// (it still passes the negative-keyword and price-floor gates).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceMapping {
    pub source: SourceId,
    /// Platform item id (Shopee `itemId`, Lazada `itemId`, retailer product code).
    #[serde(default)]
    pub item_id: Option<String>,
    /// Marketplace shop id. Required for Shopee, where an item is `(shopId, itemId)`.
    #[serde(default)]
    pub shop_id: Option<String>,
    /// Canonical product URL. Required for retailers.
    #[serde(default)]
    pub url: Option<String>,
    /// Set when the mapped page/item is a single colour.
    #[serde(default)]
    pub color: Option<ColorId>,
}

impl SourceMapping {
    pub fn validate(&self, sku: &SkuId) -> Result<(), ArbError> {
        let ctx = || format!("sku {sku} mapping {}", self.source);
        if self.item_id.is_none() && self.url.is_none() {
            return Err(ArbError::config(ctx(), "needs item_id or url"));
        }
        if self.source == SourceId::ShopeeMall && (self.item_id.is_none() || self.shop_id.is_none())
        {
            return Err(ArbError::config(
                ctx(),
                "shopee mappings need both shop_id and item_id",
            ));
        }
        if self
            .url
            .as_deref()
            .is_some_and(|url| !url.starts_with("https://"))
        {
            return Err(ArbError::config(ctx(), "url must be https"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalSku {
    pub id: SkuId,
    /// Human name for alerts: `iPhone 16 Pro Max 256GB VN/A`.
    pub display_name: String,
    pub brand: String,
    pub category: Category,
    #[serde(default)]
    pub storage_gb: Option<u32>,
    #[serde(default)]
    pub ram_gb: Option<u32>,
    /// Market variant, e.g. `VN/A`. Listings for other regions are a different SKU.
    #[serde(default)]
    pub region_code: Option<String>,
    /// Stage-2 fuzzy matching: every token must appear in the normalized title.
    #[serde(default)]
    pub required_tokens: Vec<String>,
    /// SKU-specific exclusions on top of the global negative list — typically sibling
    /// models (`"plus"`, `"16e"`, `"512gb"`) that share most tokens with this SKU.
    #[serde(default)]
    pub forbidden_tokens: Vec<String>,
    #[serde(default)]
    pub colors: Vec<ColorVariant>,
    /// Typical authorized-retail price. Seeds the trailing median for the price-floor
    /// sanity check until enough live observations exist.
    pub retail_benchmark: VndAmount,
    #[serde(default)]
    pub mappings: Vec<SourceMapping>,
    /// Marketplace keyword queries used when a source has no mapping for this SKU.
    #[serde(default)]
    pub search_queries: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl CanonicalSku {
    pub fn color(&self, id: &ColorId) -> Option<&ColorVariant> {
        self.colors.iter().find(|c| &c.id == id)
    }

    pub fn mappings_for(&self, source: SourceId) -> impl Iterator<Item = &SourceMapping> {
        self.mappings.iter().filter(move |m| m.source == source)
    }

    pub fn validate(&self) -> Result<(), ArbError> {
        let ctx = || format!("sku {}", self.id);
        if self.display_name.trim().is_empty() {
            return Err(ArbError::config(ctx(), "display_name is empty"));
        }
        if !self.retail_benchmark.is_positive() {
            return Err(ArbError::config(ctx(), "retail_benchmark must be > 0"));
        }
        if self.mappings.is_empty() && self.search_queries.is_empty() {
            return Err(ArbError::config(
                ctx(),
                "needs at least one mapping or search query",
            ));
        }
        if self.mappings.is_empty() && self.required_tokens.is_empty() {
            return Err(ArbError::config(
                ctx(),
                "search-only skus need required_tokens",
            ));
        }
        let mut seen = HashSet::new();
        for c in &self.colors {
            if !seen.insert(&c.id) {
                return Err(ArbError::config(ctx(), format!("duplicate color {}", c.id)));
            }
            if c.liquidity_bps > Bps::ONE_HUNDRED_PERCENT || c.liquidity_bps == Bps::ZERO {
                return Err(ArbError::config(
                    ctx(),
                    format!("color {} liquidity_bps must be 1..=10000", c.id),
                ));
            }
        }
        for m in &self.mappings {
            m.validate(&self.id)?;
            if let Some(color) = &m.color
                && self.color(color).is_none()
            {
                return Err(ArbError::config(
                    ctx(),
                    format!("mapping references unknown color {color}"),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum City {
    Hcmc,
    Hanoi,
    DaNang,
    Other,
}

/// What state the dealer's quote assumes the unit is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SealedCondition {
    /// Factory sealed and warranty not yet activated — the top quote.
    SealedUnactivated,
    /// Factory sealed but the retailer activated e-warranty at sale. Pays less.
    SealedActivated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteSource {
    /// From `skus.toml`.
    Config,
    /// Set live via `/set_exit`.
    Operator,
    /// A published trade-in table (worst-case liquidity baseline).
    TradeInTable,
}

/// A sealed-buyback ("thu mua nguyên seal") exit price.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuybackQuote {
    pub sku_id: SkuId,
    /// `None`: base quote for the SKU; per-colour `liquidity_bps` is applied on top.
    /// `Some`: an exact quote for that colour; no multiplier is applied.
    #[serde(default)]
    pub color: Option<ColorId>,
    pub dealer: String,
    pub city: City,
    pub price: VndAmount,
    pub condition: SealedCondition,
    /// Deducted from `price` when the purchase will not come with a VAT e-invoice.
    #[serde(default)]
    pub no_invoice_penalty: VndAmount,
    pub source: QuoteSource,
    pub quoted_at: DateTime<Utc>,
    /// Older than this and the quote is stale: still usable, but the opportunity is
    /// flagged `StaleBuybackQuote` and can never be `High` priority.
    pub max_age_secs: u64,
}

impl BuybackQuote {
    /// Seconds since the quote was taken. Negative if `quoted_at` is in the future
    /// (clock skew or a typo); callers treat negative age as stale.
    pub fn age_secs(&self, now: DateTime<Utc>) -> i64 {
        now.signed_duration_since(self.quoted_at).num_seconds()
    }

    pub fn is_stale(&self, now: DateTime<Utc>) -> bool {
        let age = self.age_secs(now);
        age < 0 || age.unsigned_abs() > self.max_age_secs
    }

    pub fn validate(&self) -> Result<(), ArbError> {
        let ctx = || format!("buyback {} @ {}", self.sku_id, self.dealer);
        if !self.price.is_positive() {
            return Err(ArbError::config(ctx(), "price must be > 0"));
        }
        if self.no_invoice_penalty.is_negative() || self.no_invoice_penalty >= self.price {
            return Err(ArbError::config(
                ctx(),
                "no_invoice_penalty must be in 0..price",
            ));
        }
        if self.max_age_secs == 0 {
            return Err(ArbError::config(ctx(), "max_age_secs must be > 0"));
        }
        Ok(())
    }
}

/// Root of `config/skus.toml`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkuCatalog {
    pub schema_version: u32,
    #[serde(default)]
    pub sku: Vec<CanonicalSku>,
    #[serde(default)]
    pub buyback: Vec<BuybackQuote>,
}

impl SkuCatalog {
    pub fn get(&self, id: &SkuId) -> Option<&CanonicalSku> {
        self.sku.iter().find(|s| &s.id == id)
    }

    pub fn enabled(&self) -> impl Iterator<Item = &CanonicalSku> {
        self.sku.iter().filter(|s| s.enabled)
    }

    pub fn quotes_for<'a>(&'a self, id: &'a SkuId) -> impl Iterator<Item = &'a BuybackQuote> {
        self.buyback.iter().filter(move |q| &q.sku_id == id)
    }

    pub fn validate(&self) -> Result<(), ArbError> {
        if self.schema_version != crate::SCHEMA_VERSION {
            return Err(ArbError::config(
                "skus.toml",
                format!(
                    "schema_version {} != supported {}",
                    self.schema_version,
                    crate::SCHEMA_VERSION
                ),
            ));
        }
        let mut ids = HashSet::new();
        for s in &self.sku {
            if !ids.insert(&s.id) {
                return Err(ArbError::config(
                    "skus.toml",
                    format!("duplicate sku {}", s.id),
                ));
            }
            s.validate()?;
        }
        for q in &self.buyback {
            q.validate()?;
            let Some(sku) = self.get(&q.sku_id) else {
                return Err(ArbError::config(
                    "skus.toml",
                    format!("buyback references unknown sku {}", q.sku_id),
                ));
            };
            if let Some(color) = &q.color
                && sku.color(color).is_none()
            {
                return Err(ArbError::config(
                    "skus.toml",
                    format!("buyback for {} references unknown color {color}", q.sku_id),
                ));
            }
        }
        for s in self.enabled() {
            if self.quotes_for(&s.id).next().is_none() {
                return Err(ArbError::config(
                    "skus.toml",
                    format!("enabled sku {} has no buyback quote", s.id),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    fn quote(quoted_at: i64, max_age: u64) -> BuybackQuote {
        BuybackQuote {
            sku_id: SkuId::new("iphone-16-pro-max-256-vna").unwrap(),
            color: None,
            dealer: "dealer-a".into(),
            city: City::Hcmc,
            price: VndAmount::try_new(29_500_000).unwrap(),
            condition: SealedCondition::SealedUnactivated,
            no_invoice_penalty: VndAmount::try_new(300_000).unwrap(),
            source: QuoteSource::Config,
            quoted_at: ts(quoted_at),
            max_age_secs: max_age,
        }
    }

    #[test]
    fn staleness() {
        let q = quote(1_000, 3_600);
        assert!(!q.is_stale(ts(1_000)));
        assert!(!q.is_stale(ts(4_600)));
        assert!(q.is_stale(ts(4_601)));
        assert!(q.is_stale(ts(999)), "future-dated quotes are stale");
        assert!(q.validate().is_ok());
    }

    #[test]
    fn catalog_rejects_dangling_buyback() {
        let cat = SkuCatalog {
            schema_version: crate::SCHEMA_VERSION,
            sku: vec![],
            buyback: vec![quote(0, 60)],
        };
        assert!(cat.validate().is_err());
    }
}
