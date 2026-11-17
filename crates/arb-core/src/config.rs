//! `EngineConfig` — the deserialized form of `config/engine.toml`.
//! Everything tunable lives here; no thresholds or intervals are hard-coded elsewhere.

use std::collections::HashSet;

use chrono::NaiveTime;
use serde::{Deserialize, Serialize};

use crate::error::ArbError;
use crate::ids::SourceId;
use crate::listing::{DeliveryEstimate, VatInvoiceStatus};
use crate::money::{Bps, SignedBps, VndAmount};
use crate::opportunity::AlertPriority;

fn default_true() -> bool {
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdMode {
    /// ROI threshold OR absolute-profit threshold.
    Any,
    /// Both.
    All,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Thresholds {
    pub min_roi_bps: Bps,
    pub min_net_profit: VndAmount,
    pub mode: ThresholdMode,
    /// Positive spreads at or above this ROI are tracked as `Watch`.
    pub watch_roi_bps: Bps,
}

impl Thresholds {
    /// Threshold decision only. `Some(High)` here may still be capped to `Normal`
    /// by the engine when a blocking `RiskFlag` is present.
    pub fn classify(&self, net_profit: VndAmount, roi: SignedBps) -> Option<AlertPriority> {
        if !net_profit.is_positive() {
            return None;
        }
        let roi_ok = roi >= self.min_roi_bps.as_signed();
        let profit_ok = net_profit >= self.min_net_profit;
        let pass = match self.mode {
            ThresholdMode::Any => roi_ok || profit_ok,
            ThresholdMode::All => roi_ok && profit_ok,
        };
        if pass {
            Some(AlertPriority::High)
        } else if roi >= self.watch_roi_bps.as_signed() {
            Some(AlertPriority::Watch)
        } else {
            None
        }
    }
}

/// A daily burst window in ICT wall-clock time, half-open `[start, end)`.
/// `start > end` means the window wraps midnight (`23:58 -> 00:15`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlashWindow {
    pub label: String,
    pub start: NaiveTime,
    pub end: NaiveTime,
}

impl FlashWindow {
    pub fn wraps_midnight(&self) -> bool {
        self.start > self.end
    }

    /// `t` is ICT wall-clock time.
    pub fn contains(&self, t: NaiveTime) -> bool {
        if self.wraps_midnight() {
            t >= self.start || t < self.end
        } else {
            t >= self.start && t < self.end
        }
    }
}

/// A calendar day (ICT) on which burst windows are armed. `month = 0` = every month.
/// The midnight window belongs to the day it ENDS on: for 11/11 the `23:58` start on
/// 10 Nov is armed because the window's end falls on a campaign day.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignDay {
    pub month: u8,
    pub day: u8,
}

impl CampaignDay {
    pub fn matches(&self, month: u32, day: u32) -> bool {
        (self.month == 0 || u32::from(self.month) == month) && u32::from(self.day) == day
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleConfig {
    /// Per-(sku, source) poll period outside burst windows.
    pub base_interval_secs: u32,
    /// Target period inside burst windows. A target, not a promise: the per-domain
    /// rate limit always wins and the scheduler degrades by priority.
    pub burst_interval_secs: u32,
    /// Uniform jitter applied to every scheduled poll, as a fraction of the interval.
    pub jitter_bps: Bps,
    pub windows: Vec<FlashWindow>,
    /// Empty = burst windows are armed every day.
    #[serde(default)]
    pub campaign_days: Vec<CampaignDay>,
}

impl ScheduleConfig {
    pub fn in_window(&self, ict_time: NaiveTime) -> Option<&FlashWindow> {
        self.windows.iter().find(|w| w.contains(ict_time))
    }

    pub fn is_campaign_day(&self, month: u32, day: u32) -> bool {
        self.campaign_days.is_empty() || self.campaign_days.iter().any(|c| c.matches(month, day))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecayTier {
    /// Applies when the delivery estimate's `max_days <= max_delivery_days`.
    pub max_delivery_days: u8,
    pub haircut_bps: Bps,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecayConfig {
    /// Ascending by `max_delivery_days`.
    pub tiers: Vec<DecayTier>,
    /// Used when delivery exceeds every tier or is unknown. Must be >= the last tier.
    pub fallback_haircut_bps: Bps,
    /// Deliveries slower than this get `RiskFlag::LongDelivery`.
    pub long_delivery_days: u8,
}

impl DecayConfig {
    /// Haircut for a delivery estimate; `None` (unknown) gets the fallback.
    pub fn haircut_for(&self, delivery: Option<DeliveryEstimate>) -> Bps {
        delivery
            .and_then(|d| {
                self.tiers
                    .iter()
                    .find(|t| d.max_days <= t.max_delivery_days)
            })
            .map_or(self.fallback_haircut_bps, |t| t.haircut_bps)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterConfig {
    /// Global exclusion tokens, matched against the normalized title on token
    /// boundaries, diacritics preserved (so `"cũ"` never matches `"củ"` or `"cu"`).
    pub negative_tokens: Vec<String>,
    /// Reject listings priced below this fraction of the trailing median.
    pub price_floor_ratio_bps: Bps,
    /// Observations kept per SKU for the trailing median.
    pub median_window: u32,
    /// Below this many live observations, `CanonicalSku::retail_benchmark` is the median.
    pub median_min_samples: u32,
    pub require_authorized_seller: bool,
    /// Stock counts at or below this get `RiskFlag::LowStock`.
    pub low_stock_threshold: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DedupConfig {
    pub bucket_width: VndAmount,
    pub cooldown_secs: u32,
    /// Re-alert inside the cooldown only if effective cost fell by at least this much
    /// relative to the last alerted cost for the same (sku, colour, source).
    pub realert_drop_bps: Bps,
    pub max_entries: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CashbackMode {
    /// No cashback in `C_eff`. The safe default: see the affiliate self-purchase note
    /// in `docs/01-research-brief.md`.
    Disabled,
    /// Use the commission rate reported on the listing.
    ListedCommission,
    /// Use `fixed_bps` for every eligible source (third-party cashback programme).
    Fixed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CashbackConfig {
    pub mode: CashbackMode,
    #[serde(default)]
    pub fixed_bps: Bps,
    /// Per-order cashback cap, if the programme has one.
    #[serde(default)]
    pub cap: Option<VndAmount>,
    /// Probability-style haircut on the nominal rebate (`7000` = count 70% of it).
    pub realization_bps: Bps,
    /// Sources cashback applies to.
    #[serde(default)]
    pub sources: Vec<SourceId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfig {
    pub source: SourceId,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Sustained request budget for the source's rate-limit domain.
    pub requests_per_minute: u32,
    /// Token-bucket burst size.
    pub burst: u32,
    pub max_concurrency: u32,
    pub timeout_ms: u32,
    /// Consecutive circuit-tripping failures before the breaker opens.
    pub breaker_failure_threshold: u32,
    pub breaker_open_secs: u32,
    /// Override the API/site origin (tests point this at `wiremock`).
    #[serde(default)]
    pub base_url: Option<String>,
    /// Applied when a listing does not state its shipping fee.
    pub default_shipping_fee: VndAmount,
    /// Applied when a listing does not state a delivery estimate. `None` = unknown.
    #[serde(default)]
    pub default_delivery: Option<DeliveryEstimate>,
    /// Applied when a listing reports `VatInvoiceStatus::Unknown`.
    pub default_vat_invoice: VatInvoiceStatus,
    /// Retailer activates e-warranty at the point of sale.
    #[serde(default)]
    pub activates_warranty_at_sale: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelConfig {
    /// Lossless alert bus capacity; senders await when full.
    pub alert_capacity: u32,
    /// Lossy event bus capacity; ticks are dropped (and counted) when full.
    pub event_capacity: u32,
    /// Also publish every raw listing as `EngineEvent::Observation` so a session can
    /// be replayed later. Roughly 1 KB per poll result: turn on for campaign days.
    #[serde(default)]
    pub record_observations: bool,
}

/// Root of `config/engine.toml`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineConfig {
    pub schema_version: u32,
    /// Sent as `User-Agent` on retailer requests. Must identify the bot and a contact.
    pub user_agent: String,
    pub thresholds: Thresholds,
    pub schedule: ScheduleConfig,
    pub decay: DecayConfig,
    pub filter: FilterConfig,
    pub dedup: DedupConfig,
    pub cashback: CashbackConfig,
    pub channels: ChannelConfig,
    pub source: Vec<SourceConfig>,
}

impl EngineConfig {
    pub fn source(&self, id: SourceId) -> Option<&SourceConfig> {
        self.source.iter().find(|s| s.source == id)
    }

    pub fn validate(&self) -> Result<(), ArbError> {
        let err = |m: &str| Err(ArbError::config("engine.toml", m));
        if self.schema_version != crate::SCHEMA_VERSION {
            return err("unsupported schema_version");
        }
        if self.user_agent.trim().is_empty() {
            return err("user_agent is empty");
        }

        let s = &self.schedule;
        if s.burst_interval_secs == 0 || s.base_interval_secs < s.burst_interval_secs {
            return err("need 0 < burst_interval_secs <= base_interval_secs");
        }
        if s.jitter_bps > Bps::ONE_HUNDRED_PERCENT {
            return err("jitter_bps must be <= 10000");
        }
        if s.windows.iter().any(|w| w.start == w.end) {
            return err("flash window start == end");
        }
        if s.campaign_days
            .iter()
            .any(|c| c.month > 12 || c.day == 0 || c.day > 31)
        {
            return err("campaign day out of range");
        }

        let d = &self.decay;
        if d.tiers.is_empty() {
            return err("decay.tiers is empty");
        }
        let ascending = d
            .tiers
            .windows(2)
            .all(|w| matches!(w, [a, b] if a.max_delivery_days < b.max_delivery_days && a.haircut_bps <= b.haircut_bps));
        if !ascending {
            return err("decay.tiers must ascend in max_delivery_days and haircut_bps");
        }
        if d.tiers
            .iter()
            .any(|t| t.haircut_bps > d.fallback_haircut_bps)
            || d.fallback_haircut_bps >= Bps::ONE_HUNDRED_PERCENT
        {
            return err("decay.fallback_haircut_bps must be >= every tier and < 10000");
        }

        let f = &self.filter;
        if f.price_floor_ratio_bps == Bps::ZERO
            || f.price_floor_ratio_bps >= Bps::ONE_HUNDRED_PERCENT
        {
            return err("filter.price_floor_ratio_bps must be in 1..10000");
        }
        if f.median_window == 0
            || f.median_min_samples == 0
            || f.median_min_samples > f.median_window
        {
            return err("need 0 < median_min_samples <= median_window");
        }

        let dd = &self.dedup;
        if !dd.bucket_width.is_positive() || dd.max_entries == 0 {
            return err("dedup.bucket_width and max_entries must be > 0");
        }

        let c = &self.cashback;
        if c.realization_bps > Bps::ONE_HUNDRED_PERCENT {
            return err("cashback.realization_bps must be <= 10000");
        }
        if c.mode == CashbackMode::Fixed && c.fixed_bps == Bps::ZERO {
            return err("cashback.mode = fixed needs fixed_bps > 0");
        }
        if c.cap.is_some_and(|cap| !cap.is_positive()) {
            return err("cashback.cap must be > 0 when present");
        }

        if self.channels.alert_capacity == 0 || self.channels.event_capacity == 0 {
            return err("channel capacities must be > 0");
        }

        let mut seen = HashSet::new();
        for src in &self.source {
            if !seen.insert(src.source) {
                return err("duplicate [[source]] entry");
            }
            if src.requests_per_minute == 0
                || src.burst == 0
                || src.max_concurrency == 0
                || src.timeout_ms == 0
            {
                return err("source rate/concurrency/timeout values must be > 0");
            }
            if src.breaker_failure_threshold == 0 || src.breaker_open_secs == 0 {
                return err("source breaker values must be > 0");
            }
            if src.default_shipping_fee.is_negative() {
                return err("source default_shipping_fee must be >= 0");
            }
            if let Some(delivery) = src.default_delivery {
                delivery.validate()?;
            }
        }
        if !self.source.iter().any(|s| s.enabled) {
            return err("no enabled source");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn vnd(d: i64) -> VndAmount {
        VndAmount::try_new(d).unwrap()
    }

    fn bps(b: u32) -> Bps {
        Bps::try_new(b).unwrap()
    }

    #[test]
    fn midnight_window_wraps() {
        let w = FlashWindow {
            label: "midnight".into(),
            start: t(23, 58),
            end: t(0, 15),
        };
        assert!(w.wraps_midnight());
        assert!(w.contains(t(23, 58)));
        assert!(w.contains(t(0, 0)));
        assert!(w.contains(t(0, 14)));
        assert!(!w.contains(t(0, 15)));
        assert!(!w.contains(t(23, 57)));
        let noon = FlashWindow {
            label: "noon".into(),
            start: t(11, 58),
            end: t(12, 15),
        };
        assert!(noon.contains(t(12, 0)));
        assert!(!noon.contains(t(12, 15)));
    }

    #[test]
    fn campaign_day_wildcard_month() {
        let payday = CampaignDay { month: 0, day: 25 };
        assert!(payday.matches(3, 25));
        assert!(!payday.matches(3, 24));
        let double = CampaignDay { month: 11, day: 11 };
        assert!(double.matches(11, 11));
        assert!(!double.matches(12, 11));
    }

    #[test]
    fn threshold_modes() {
        let mut th = Thresholds {
            min_roi_bps: bps(700),
            min_net_profit: vnd(1_500_000),
            mode: ThresholdMode::Any,
            watch_roi_bps: bps(200),
        };
        // 5% ROI but 1.6M profit
        assert_eq!(
            th.classify(vnd(1_600_000), SignedBps::new(500)),
            Some(AlertPriority::High)
        );
        th.mode = ThresholdMode::All;
        assert_eq!(
            th.classify(vnd(1_600_000), SignedBps::new(500)),
            Some(AlertPriority::Watch)
        );
        assert_eq!(th.classify(vnd(100_000), SignedBps::new(100)), None);
        assert_eq!(th.classify(vnd(-1), SignedBps::new(900)), None);
    }

    #[test]
    fn decay_tier_selection() {
        let d = DecayConfig {
            tiers: vec![
                DecayTier {
                    max_delivery_days: 1,
                    haircut_bps: bps(50),
                },
                DecayTier {
                    max_delivery_days: 3,
                    haircut_bps: bps(150),
                },
                DecayTier {
                    max_delivery_days: 6,
                    haircut_bps: bps(350),
                },
            ],
            fallback_haircut_bps: bps(500),
            long_delivery_days: 5,
        };
        let est = |min_days, max_days| Some(DeliveryEstimate { min_days, max_days });
        assert_eq!(d.haircut_for(est(0, 1)), bps(50));
        assert_eq!(d.haircut_for(est(2, 3)), bps(150));
        assert_eq!(d.haircut_for(est(3, 6)), bps(350));
        assert_eq!(d.haircut_for(est(5, 9)), bps(500));
        assert_eq!(d.haircut_for(None), bps(500));
    }
}
