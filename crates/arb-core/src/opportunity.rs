//! Engine outputs: the audited arithmetic behind a spread, the opportunity itself,
//! and the events that flow to storage and Telegram.
//!
//! The two `build` constructors are the single definition of the pricing identities.
//! The engine decides *which* vouchers/quote/haircut apply; these functions decide
//! how the numbers combine. Replay, alerts and tests all go through them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::ArbError;
use crate::ids::{ColorId, SkuId, SourceId};
use crate::listing::{Availability, RawMarketListing};
use crate::money::{Bps, Rounding, SignedBps, VndAmount};
use crate::sku::BuybackQuote;
use crate::voucher::VoucherLayer;

/// A voucher the engine assumed, with the amount it contributed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedVoucher {
    pub rule_id: String,
    pub label: String,
    pub layer: VoucherLayer,
    pub amount: VndAmount,
    pub contested: bool,
}

/// `C_eff = listed - shop - platform - payment - cashback + shipping_net`
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostBreakdown {
    pub listed_price: VndAmount,
    pub shop_voucher: VndAmount,
    pub platform_voucher: VndAmount,
    pub payment_discount: VndAmount,
    /// Expected affiliate/cashback rebate, already haircut for realization risk.
    pub expected_cashback: VndAmount,
    /// Shipping + insurance before any shipping voucher.
    pub shipping_gross: VndAmount,
    pub shipping_discount: VndAmount,
    /// `max(shipping_gross - shipping_discount, 0)`.
    pub shipping_net: VndAmount,
    pub effective_cost: VndAmount,
    pub applied: Vec<AppliedVoucher>,
}

impl CostBreakdown {
    /// Combine already-selected vouchers into `C_eff`. Fails if any input is negative
    /// or the result is not strictly positive (a "free" phone is a data error).
    pub fn build(
        listed_price: VndAmount,
        applied: Vec<AppliedVoucher>,
        expected_cashback: VndAmount,
        shipping_gross: VndAmount,
    ) -> Result<Self, ArbError> {
        if !listed_price.is_positive()
            || expected_cashback.is_negative()
            || shipping_gross.is_negative()
            || applied.iter().any(|a| a.amount.is_negative())
        {
            return Err(ArbError::Arithmetic {
                op: "cost_breakdown_negative_input",
            });
        }
        let layer_total = |layer: VoucherLayer| {
            VndAmount::sum_checked(
                applied
                    .iter()
                    .filter(|a| a.layer == layer)
                    .map(|a| a.amount),
            )
        };
        let shop_voucher = layer_total(VoucherLayer::Shop)?;
        let platform_voucher = layer_total(VoucherLayer::Platform)?;
        let payment_discount = layer_total(VoucherLayer::Payment)?;
        let shipping_discount = layer_total(VoucherLayer::Shipping)?;
        let shipping_net = shipping_gross.sub_floor_zero(shipping_discount)?;
        let effective_cost = listed_price
            .checked_sub(shop_voucher)?
            .checked_sub(platform_voucher)?
            .checked_sub(payment_discount)?
            .checked_sub(expected_cashback)?
            .checked_add(shipping_net)?;
        if !effective_cost.is_positive() {
            return Err(ArbError::Arithmetic {
                op: "cost_breakdown_nonpositive_effective_cost",
            });
        }
        Ok(Self {
            listed_price,
            shop_voucher,
            platform_voucher,
            payment_discount,
            expected_cashback,
            shipping_gross,
            shipping_discount,
            shipping_net,
            effective_cost,
            applied,
        })
    }

    /// Cash that actually leaves the account at checkout. Cashback arrives weeks
    /// later (if at all), so this is the capital at risk.
    pub fn cash_outlay(&self) -> Result<VndAmount, ArbError> {
        self.effective_cost.checked_add(self.expected_cashback)
    }

    pub fn has_contested(&self) -> bool {
        self.applied.iter().any(|a| a.contested)
    }
}

/// `P_exit = floor(quote * colour) - invoice_penalty - ceil(adjusted * decay)`
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitBreakdown {
    pub quote: BuybackQuote,
    /// `10000` when the quote is colour-specific or the colour is unknown-but-neutral.
    pub color_liquidity: Bps,
    pub invoice_penalty: VndAmount,
    /// Quote after colour and invoice adjustments, before decay.
    pub adjusted_quote: VndAmount,
    pub decay_haircut: Bps,
    pub decay_amount: VndAmount,
    pub exit_price: VndAmount,
    pub quote_age_secs: i64,
    pub quote_stale: bool,
}

impl ExitBreakdown {
    /// * `color_liquidity` — multiplier for the listing's colour (`<= 10000`).
    /// * `invoice_expected` — whether the purchase will come with a VAT e-invoice;
    ///   if not, `quote.no_invoice_penalty` is deducted.
    /// * `decay_haircut` — delivery price-decay buffer from `DecayConfig`.
    pub fn build(
        quote: BuybackQuote,
        color_liquidity: Bps,
        invoice_expected: bool,
        decay_haircut: Bps,
        now: DateTime<Utc>,
    ) -> Result<Self, ArbError> {
        if color_liquidity > Bps::ONE_HUNDRED_PERCENT || decay_haircut >= Bps::ONE_HUNDRED_PERCENT {
            return Err(ArbError::Arithmetic {
                op: "exit_breakdown_rate_out_of_range",
            });
        }
        let invoice_penalty = if invoice_expected {
            VndAmount::ZERO
        } else {
            quote.no_invoice_penalty
        };
        let adjusted_quote = quote
            .price
            .mul_bps(color_liquidity, Rounding::Down)?
            .checked_sub(invoice_penalty)?;
        let decay_amount = adjusted_quote.mul_bps(decay_haircut, Rounding::Up)?;
        let exit_price = adjusted_quote.checked_sub(decay_amount)?;
        if !exit_price.is_positive() {
            return Err(ArbError::Arithmetic {
                op: "exit_breakdown_nonpositive_exit",
            });
        }
        Ok(Self {
            quote_age_secs: quote.age_secs(now),
            quote_stale: quote.is_stale(now),
            quote,
            color_liquidity,
            invoice_penalty,
            adjusted_quote,
            decay_haircut,
            decay_amount,
            exit_price,
        })
    }
}

/// `(P_exit - C_eff, floor((P_exit - C_eff) / C_eff))`.
pub fn net_profit_and_roi(
    effective_cost: VndAmount,
    exit_price: VndAmount,
) -> Result<(VndAmount, SignedBps), ArbError> {
    let net = exit_price.checked_sub(effective_cost)?;
    let roi = net.ratio_bps(effective_cost)?;
    Ok((net, roi))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertPriority {
    /// Positive but below threshold; surfaced in `/top_spreads` only.
    Watch,
    /// Meets the threshold but carries a blocking risk flag.
    Normal,
    /// Meets the threshold with no blocking risk flag. Pushes with sound.
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskFlag {
    NoVatInvoice,
    VatInvoiceUnknown,
    StaleBuybackQuote,
    /// Delivery estimate missing; the worst-case decay tier was applied.
    DeliveryUnknown,
    LongDelivery,
    LowStock,
    StockUnknown,
    /// At least one contested voucher was assumed; see `net_profit_if_contested_fail`.
    ContestedVoucherAssumed,
    /// Profit turns non-positive if contested vouchers fail.
    UnprofitableWithoutContested,
    /// Retailer is known to activate e-warranty at sale; quote assumes unactivated.
    WarrantyActivationRisk,
    /// Price came from the CSS fallback, not structured data.
    LowTrustExtraction,
    PreOrder,
    /// Listing observation is older than one polling interval.
    StaleObservation,
}

impl RiskFlag {
    /// Flags that cap an opportunity at `Normal` priority.
    pub const fn blocks_high_priority(self) -> bool {
        matches!(
            self,
            Self::StaleBuybackQuote
                | Self::UnprofitableWithoutContested
                | Self::LowTrustExtraction
                | Self::PreOrder
                | Self::StaleObservation
        )
    }
}

/// Dedup key: `(sku, colour, source, effective-cost bucket)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AlertFingerprint {
    pub sku_id: SkuId,
    pub color: Option<ColorId>,
    pub source: SourceId,
    /// `effective_cost.bucket_floor(dedup.bucket_width)`.
    pub price_bucket: VndAmount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArbitrageOpportunity {
    pub sku_id: SkuId,
    pub sku_display: String,
    pub color: Option<ColorId>,
    pub color_display: Option<String>,
    pub source: SourceId,
    pub listing: RawMarketListing,
    pub cost: CostBreakdown,
    pub exit: ExitBreakdown,
    pub net_profit: VndAmount,
    pub roi: SignedBps,
    /// Same spread recomputed with every contested voucher removed.
    /// Equals `net_profit` when no contested voucher was applied.
    pub net_profit_if_contested_fail: VndAmount,
    pub priority: AlertPriority,
    pub risk_flags: Vec<RiskFlag>,
    pub fingerprint: AlertFingerprint,
    pub detected_at: DateTime<Utc>,
    /// `detected_at - listing.observed_at`: in-process latency, not market latency.
    pub pipeline_latency_ms: u64,
}

impl ArbitrageOpportunity {
    /// Link to put behind the alert button: affiliate link if present, else canonical.
    pub fn deep_link(&self) -> &str {
        self.listing
            .affiliate_url
            .as_deref()
            .unwrap_or(&self.listing.url)
    }

    pub fn has_flag(&self, flag: RiskFlag) -> bool {
        self.risk_flags.contains(&flag)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AlertKind {
    /// First time this fingerprint is seen (or its entry was evicted).
    New,
    /// Same SKU/source re-alerted because cost fell by at least `realert_drop_bps`.
    PriceDrop { previous_effective_cost: VndAmount },
    /// Still valid after `cooldown_secs`.
    CooldownExpired,
}

/// What the dedup stage emits to the alert bus. Never dropped under backpressure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertEvent {
    pub kind: AlertKind,
    pub opportunity: ArbitrageOpportunity,
}

/// Compact price-history row. One per accepted listing per poll.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceTick {
    pub sku_id: SkuId,
    pub color: Option<ColorId>,
    pub source: SourceId,
    pub platform_item_id: String,
    pub listed_price: VndAmount,
    /// `None` when evaluation failed (e.g. no usable quote).
    pub effective_cost: Option<VndAmount>,
    pub exit_price: Option<VndAmount>,
    pub availability: Availability,
    pub observed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum RejectReason {
    NegativeKeyword {
        token: String,
    },
    ForbiddenToken {
        token: String,
    },
    MissingRequiredToken {
        token: String,
    },
    UnauthorizedSeller,
    ConditionNotNew,
    NotPurchasable,
    /// Below `price_floor_ratio_bps` of the trailing median: accessory or glitch.
    BelowPriceFloor {
        listed_price: VndAmount,
        median: VndAmount,
    },
    AmbiguousColor,
    NoSkuMatch,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedListing {
    pub reason: RejectReason,
    pub sku_id: Option<SkuId>,
    pub listing: RawMarketListing,
    pub rejected_at: DateTime<Utc>,
}

/// Everything the engine publishes on the (lossy, bounded) event bus for persistence.
/// Alerts travel separately on the lossless alert bus.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    /// The raw listing exactly as the connector returned it, before any filtering.
    /// Emitted only when `channels.record_observations` is on; this is what the
    /// replay simulator consumes.
    Observation {
        requested_sku: SkuId,
        listing: Box<RawMarketListing>,
    },
    Tick(PriceTick),
    /// Every evaluated spread with positive net profit, alerted or not.
    Spread(Box<ArbitrageOpportunity>),
    Rejected(Box<RejectedListing>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sku::{City, QuoteSource, SealedCondition};

    fn vnd(d: i64) -> VndAmount {
        VndAmount::try_new(d).unwrap()
    }

    fn bps(b: u32) -> Bps {
        Bps::try_new(b).unwrap()
    }

    fn ts(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    fn applied(layer: VoucherLayer, amount: i64, contested: bool) -> AppliedVoucher {
        AppliedVoucher {
            rule_id: "r".into(),
            label: "l".into(),
            layer,
            amount: vnd(amount),
            contested,
        }
    }

    fn quote() -> BuybackQuote {
        BuybackQuote {
            sku_id: SkuId::new("iphone-16-pro-max-256-vna").unwrap(),
            color: None,
            dealer: "dealer-a".into(),
            city: City::Hcmc,
            price: vnd(30_000_000),
            condition: SealedCondition::SealedUnactivated,
            no_invoice_penalty: vnd(300_000),
            source: QuoteSource::Config,
            quoted_at: ts(0),
            max_age_secs: 3_600,
        }
    }

    #[test]
    fn cost_identity() {
        let c = CostBreakdown::build(
            vnd(30_990_000),
            vec![
                applied(VoucherLayer::Shop, 500_000, false),
                applied(VoucherLayer::Platform, 1_000_000, true),
                applied(VoucherLayer::Payment, 300_000, false),
                applied(VoucherLayer::Shipping, 50_000, false),
            ],
            vnd(600_000),
            vnd(30_000),
        )
        .unwrap();
        // shipping voucher exceeds the fee: net shipping floors at zero
        assert_eq!(c.shipping_net, VndAmount::ZERO);
        assert_eq!(
            c.effective_cost,
            vnd(30_990_000 - 500_000 - 1_000_000 - 300_000 - 600_000)
        );
        assert_eq!(c.cash_outlay().unwrap(), vnd(29_190_000));
        assert!(c.has_contested());
    }

    #[test]
    fn cost_rejects_nonsense() {
        assert!(
            CostBreakdown::build(VndAmount::ZERO, vec![], VndAmount::ZERO, VndAmount::ZERO)
                .is_err()
        );
        let free = CostBreakdown::build(
            vnd(100),
            vec![applied(VoucherLayer::Shop, 100, false)],
            VndAmount::ZERO,
            VndAmount::ZERO,
        );
        assert!(free.is_err());
    }

    #[test]
    fn exit_identity_rounds_against_us() {
        // 30_000_000 * 97% = 29_100_000; - 300_000 = 28_800_000; 2.5% decay = 720_000
        let e = ExitBreakdown::build(quote(), bps(9_700), false, bps(250), ts(60)).unwrap();
        assert_eq!(e.adjusted_quote, vnd(28_800_000));
        assert_eq!(e.decay_amount, vnd(720_000));
        assert_eq!(e.exit_price, vnd(28_080_000));
        assert!(!e.quote_stale);

        // odd numbers: decay rounds UP
        let mut q = quote();
        q.price = vnd(29_999_999);
        let e = ExitBreakdown::build(q, bps(10_000), true, bps(333), ts(7_200)).unwrap();
        // 29_999_999 * 3.33% = 998_999.9667 -> 999_000
        assert_eq!(e.decay_amount, vnd(999_000));
        assert!(e.quote_stale);
        assert!(ExitBreakdown::build(quote(), bps(10_001), true, bps(0), ts(0)).is_err());
    }

    #[test]
    fn profit_and_roi() {
        let (net, roi) = net_profit_and_roi(vnd(26_000_000), vnd(28_080_000)).unwrap();
        assert_eq!(net, vnd(2_080_000));
        assert_eq!(roi, SignedBps::new(800));
        let (net, roi) = net_profit_and_roi(vnd(30_000_000), vnd(28_080_000)).unwrap();
        assert!(net.is_negative());
        assert_eq!(roi, SignedBps::new(-640));
    }
}
