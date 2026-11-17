//! Voucher rules. `VoucherBook` is the deserialized form of `config/vouchers_*.toml`
//! and the payload of the live `/vouchers` command.
//!
//! Stacking model (enforced by the engine, stated here because it is a contract):
//! at most ONE rule per `VoucherLayer` applies to an order, and layers apply in the
//! declared order `Shop -> Platform -> Payment -> Shipping`. Within a layer the engine
//! picks the rule with the largest discount for that listing.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::ArbError;
use crate::ids::{SkuId, SourceId};
use crate::money::{Bps, Rounding, VndAmount};
use crate::sku::Category;

fn default_true() -> bool {
    true
}

/// Declaration order is application order (`Ord` is derived and relied upon).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoucherLayer {
    Shop,
    Platform,
    Payment,
    /// Reduces the shipping fee only; never the item price.
    Shipping,
}

impl VoucherLayer {
    pub const ALL: [Self; 4] = [Self::Shop, Self::Platform, Self::Payment, Self::Shipping];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentMethod {
    VnpayQr,
    CreditCard,
    ShopeePay,
    SPayLater,
    ZaloPay,
    Momo,
    BankTransfer,
    Cod,
}

/// What the percentage and the minimum spend are measured against.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscountBasis {
    /// The listed price, ignoring discounts from earlier layers.
    #[default]
    ListedPrice,
    /// The running subtotal after earlier layers were applied.
    AfterPriorLayers,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiscountKind {
    Fixed {
        amount: VndAmount,
    },
    /// "Giảm 8% tối đa 1.000.000₫". An uncapped percentage must say so explicitly
    /// with `cap = None`; on high-ticket SKUs the cap is nearly always what binds.
    Percent {
        rate_bps: Bps,
        #[serde(default)]
        cap: Option<VndAmount>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoucherRule {
    /// Unique within a book. Shown in alerts so the operator knows what was assumed.
    pub id: String,
    pub label: String,
    pub layer: VoucherLayer,
    /// Sources this rule can apply on. Must be non-empty.
    pub sources: Vec<SourceId>,
    pub discount: DiscountKind,
    #[serde(default)]
    pub basis: DiscountBasis,
    #[serde(default)]
    pub min_spend: VndAmount,
    pub valid_from: DateTime<Utc>,
    pub valid_until: DateTime<Utc>,
    /// Empty = every SKU.
    #[serde(default)]
    pub eligible_skus: Vec<SkuId>,
    /// Empty = every category.
    #[serde(default)]
    pub eligible_categories: Vec<Category>,
    /// Only applies to Official Mall listings.
    #[serde(default)]
    pub mall_only: bool,
    /// For `Payment` layer rules: the method the buyer must pay with.
    #[serde(default)]
    pub payment_method: Option<PaymentMethod>,
    /// Limited-quantity voucher that sells out in seconds at 00:00. The engine reports
    /// a second profit figure assuming every contested voucher FAILS to apply.
    #[serde(default)]
    pub contested: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl VoucherRule {
    /// Half-open validity window `[valid_from, valid_until)`.
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        self.enabled && now >= self.valid_from && now < self.valid_until
    }

    /// Static eligibility: source, SKU, category, mall. Time, min-spend and layer
    /// competition are evaluated by the engine.
    pub fn is_eligible(
        &self,
        source: SourceId,
        sku: &SkuId,
        category: Category,
        is_mall: bool,
    ) -> bool {
        self.sources.contains(&source)
            && (self.eligible_skus.is_empty() || self.eligible_skus.contains(sku))
            && (self.eligible_categories.is_empty() || self.eligible_categories.contains(&category))
            && (!self.mall_only || is_mall)
    }

    /// Discount this rule yields on `basis_amount`, or zero if `min_spend` is not met.
    /// Rounds DOWN (never overstates a discount) and never exceeds `basis_amount`.
    pub fn discount_on(&self, basis_amount: VndAmount) -> Result<VndAmount, ArbError> {
        if basis_amount < self.min_spend || !basis_amount.is_positive() {
            return Ok(VndAmount::ZERO);
        }
        let raw = match self.discount {
            DiscountKind::Fixed { amount } => amount,
            DiscountKind::Percent { rate_bps, cap } => {
                let pct = basis_amount.mul_bps(rate_bps, Rounding::Down)?;
                cap.map_or(pct, |c| pct.min(c))
            }
        };
        Ok(raw.min(basis_amount).max(VndAmount::ZERO))
    }

    pub fn validate(&self) -> Result<(), ArbError> {
        let ctx = || format!("voucher {}", self.id);
        if self.id.trim().is_empty() {
            return Err(ArbError::config("voucher", "id is empty"));
        }
        if self.sources.is_empty() {
            return Err(ArbError::config(ctx(), "sources is empty"));
        }
        if self.valid_until <= self.valid_from {
            return Err(ArbError::config(
                ctx(),
                "valid_until must be after valid_from",
            ));
        }
        if self.min_spend.is_negative() {
            return Err(ArbError::config(ctx(), "min_spend must be >= 0"));
        }
        match self.discount {
            DiscountKind::Fixed { amount } if !amount.is_positive() => {
                return Err(ArbError::config(ctx(), "fixed amount must be > 0"));
            }
            DiscountKind::Percent { rate_bps, cap } => {
                if rate_bps == Bps::ZERO || rate_bps > Bps::ONE_HUNDRED_PERCENT {
                    return Err(ArbError::config(ctx(), "rate_bps must be 1..=10000"));
                }
                if cap.is_some_and(|c| !c.is_positive()) {
                    return Err(ArbError::config(ctx(), "cap must be > 0 when present"));
                }
            }
            DiscountKind::Fixed { .. } => {}
        }
        match (self.layer, self.payment_method) {
            (VoucherLayer::Payment, None) => Err(ArbError::config(
                ctx(),
                "payment layer needs payment_method",
            )),
            (layer, Some(_)) if layer != VoucherLayer::Payment => Err(ArbError::config(
                ctx(),
                "payment_method only allowed on the payment layer",
            )),
            _ => Ok(()),
        }
    }
}

/// Root of `config/vouchers_*.toml`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoucherBook {
    pub schema_version: u32,
    /// Free-form campaign label, e.g. `11.11-2026`.
    pub campaign: String,
    /// Payment methods the operator can actually use. Payment-layer rules for any
    /// other method are ignored — a card you do not hold is not a discount.
    #[serde(default)]
    pub available_payment_methods: Vec<PaymentMethod>,
    #[serde(default)]
    pub voucher: Vec<VoucherRule>,
}

impl VoucherBook {
    pub fn active_at(&self, now: DateTime<Utc>) -> impl Iterator<Item = &VoucherRule> {
        self.voucher.iter().filter(move |v| v.is_active(now))
    }

    pub fn validate(&self) -> Result<(), ArbError> {
        if self.schema_version != crate::SCHEMA_VERSION {
            return Err(ArbError::config(
                "vouchers",
                format!(
                    "schema_version {} != supported {}",
                    self.schema_version,
                    crate::SCHEMA_VERSION
                ),
            ));
        }
        let mut ids = HashSet::new();
        for v in &self.voucher {
            v.validate()?;
            if !ids.insert(v.id.as_str()) {
                return Err(ArbError::config(
                    "vouchers",
                    format!("duplicate voucher id {}", v.id),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vnd(d: i64) -> VndAmount {
        VndAmount::try_new(d).unwrap()
    }

    fn ts(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    fn rule(discount: DiscountKind, min_spend: i64) -> VoucherRule {
        VoucherRule {
            id: "v1".into(),
            label: "test".into(),
            layer: VoucherLayer::Platform,
            sources: vec![SourceId::ShopeeMall],
            discount,
            basis: DiscountBasis::ListedPrice,
            min_spend: vnd(min_spend),
            valid_from: ts(100),
            valid_until: ts(200),
            eligible_skus: vec![],
            eligible_categories: vec![],
            mall_only: true,
            payment_method: None,
            contested: false,
            enabled: true,
        }
    }

    #[test]
    fn percent_is_capped_and_gated_by_min_spend() {
        let r = rule(
            DiscountKind::Percent {
                rate_bps: Bps::try_new(800).unwrap(),
                cap: Some(vnd(1_000_000)),
            },
            10_000_000,
        );
        assert_eq!(r.discount_on(vnd(34_990_000)).unwrap(), vnd(1_000_000));
        assert_eq!(r.discount_on(vnd(10_000_000)).unwrap(), vnd(800_000));
        assert_eq!(r.discount_on(vnd(9_999_999)).unwrap(), VndAmount::ZERO);
        assert!(r.validate().is_ok());
    }

    #[test]
    fn fixed_never_exceeds_basis() {
        let r = rule(
            DiscountKind::Fixed {
                amount: vnd(500_000),
            },
            0,
        );
        assert_eq!(r.discount_on(vnd(300_000)).unwrap(), vnd(300_000));
        assert_eq!(r.discount_on(VndAmount::ZERO).unwrap(), VndAmount::ZERO);
    }

    #[test]
    fn window_is_half_open_and_eligibility_respects_mall() {
        let r = rule(DiscountKind::Fixed { amount: vnd(1) }, 0);
        assert!(!r.is_active(ts(99)));
        assert!(r.is_active(ts(100)));
        assert!(!r.is_active(ts(200)));
        let sku = SkuId::new("x").unwrap();
        assert!(r.is_eligible(SourceId::ShopeeMall, &sku, Category::Phone, true));
        assert!(!r.is_eligible(SourceId::ShopeeMall, &sku, Category::Phone, false));
        assert!(!r.is_eligible(SourceId::LazMall, &sku, Category::Phone, true));
    }

    #[test]
    fn payment_layer_requires_method() {
        let mut r = rule(DiscountKind::Fixed { amount: vnd(1) }, 0);
        r.layer = VoucherLayer::Payment;
        assert!(r.validate().is_err());
        r.payment_method = Some(PaymentMethod::VnpayQr);
        assert!(r.validate().is_ok());
    }
}
