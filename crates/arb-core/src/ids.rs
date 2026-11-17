//! Identifiers. Slugs are validated on construction *and* on deserialization, so an
//! id that exists is always safe to use as a map key, log field or Telegram argument.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::ArbError;

const MAX_SLUG_LEN: usize = 64;

fn is_valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_SLUG_LEN
        && !s.starts_with('-')
        && !s.ends_with('-')
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

macro_rules! slug_id {
    ($(#[$meta:meta])* $name:ident, $kind:literal) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Accepts `[a-z0-9-]{1,64}` with no leading/trailing hyphen.
            pub fn new(raw: impl Into<String>) -> Result<Self, ArbError> {
                let raw = raw.into();
                if is_valid_slug(&raw) {
                    Ok(Self(raw))
                } else {
                    Err(ArbError::InvalidIdentifier { kind: $kind, value: raw })
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = ArbError;
            fn try_from(raw: String) -> Result<Self, ArbError> {
                Self::new(raw)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

slug_id!(
    /// Canonical SKU slug, e.g. `iphone-16-pro-max-256-vna`. Colour is NOT part of it.
    SkuId,
    "sku"
);

slug_id!(
    /// Colour slug scoped to a SKU, e.g. `natural-titanium`.
    ColorId,
    "color"
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Authorized retail chain; public product pages.
    Retailer,
    /// Marketplace reached through an official affiliate/open API.
    Marketplace,
}

/// Every data source the system knows. Adding one is a contract change (new variant +
/// new connector), which is intentional: sources are compliance-reviewed, not ad hoc.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SourceId {
    #[serde(rename = "tgdd")]
    Tgdd,
    #[serde(rename = "cellphones")]
    CellphoneS,
    #[serde(rename = "shopdunk")]
    ShopDunk,
    #[serde(rename = "fptshop")]
    FptShop,
    #[serde(rename = "hoangha")]
    HoangHa,
    #[serde(rename = "shopee_mall")]
    ShopeeMall,
    #[serde(rename = "lazmall")]
    LazMall,
}

impl SourceId {
    pub const ALL: [Self; 7] = [
        Self::Tgdd,
        Self::CellphoneS,
        Self::ShopDunk,
        Self::FptShop,
        Self::HoangHa,
        Self::ShopeeMall,
        Self::LazMall,
    ];

    /// Stable key: config files, DB rows, metric labels, Telegram callback data.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tgdd => "tgdd",
            Self::CellphoneS => "cellphones",
            Self::ShopDunk => "shopdunk",
            Self::FptShop => "fptshop",
            Self::HoangHa => "hoangha",
            Self::ShopeeMall => "shopee_mall",
            Self::LazMall => "lazmall",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Tgdd => "Thế Giới Di Động",
            Self::CellphoneS => "CellphoneS",
            Self::ShopDunk => "ShopDunk",
            Self::FptShop => "FPT Shop",
            Self::HoangHa => "Hoàng Hà Mobile",
            Self::ShopeeMall => "Shopee Mall",
            Self::LazMall => "LazMall",
        }
    }

    pub const fn kind(self) -> SourceKind {
        match self {
            Self::ShopeeMall | Self::LazMall => SourceKind::Marketplace,
            _ => SourceKind::Retailer,
        }
    }

    /// Rate-limit key. One token bucket per registrable domain, not per connector.
    pub const fn rate_limit_domain(self) -> &'static str {
        match self {
            Self::Tgdd => "thegioididong.com",
            Self::CellphoneS => "cellphones.com.vn",
            Self::ShopDunk => "shopdunk.com",
            Self::FptShop => "fptshop.com.vn",
            Self::HoangHa => "hoanghamobile.com",
            Self::ShopeeMall => "open-api.affiliate.shopee.vn",
            Self::LazMall => "api.lazada.vn",
        }
    }

    pub fn parse(s: &str) -> Result<Self, ArbError> {
        Self::ALL
            .into_iter()
            .find(|src| src.as_str() == s)
            .ok_or_else(|| ArbError::InvalidIdentifier {
                kind: "source",
                value: s.to_owned(),
            })
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_validation() {
        assert!(SkuId::new("iphone-16-pro-max-256-vna").is_ok());
        let too_long = "a".repeat(65);
        for bad in ["", "-x", "x-", "iPhone", "a b", "ốp", too_long.as_str()] {
            assert!(SkuId::new(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn source_keys_round_trip_and_are_unique() {
        for s in SourceId::ALL {
            assert_eq!(SourceId::parse(s.as_str()).unwrap(), s);
        }
        let mut keys: Vec<_> = SourceId::ALL.iter().map(|s| s.as_str()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), SourceId::ALL.len());
        assert!(SourceId::parse("tiki").is_err());
    }
}
