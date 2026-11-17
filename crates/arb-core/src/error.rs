//! Unified error taxonomy.
//!
//! Design rules:
//! - `Clone + Eq`, no wrapped foreign error types: adapters stringify `reqwest`/`sqlx`
//!   errors at the boundary. That keeps `arb-core` free of heavy deps and lets errors
//!   cross channels and be asserted on in tests.
//! - The connector field is called `source_id`, never `source` — `thiserror` treats a
//!   field named `source` as the error's cause and would require it to be `Error`.

use std::fmt;

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::ids::{SkuId, SourceId};

/// Where in the extraction pipeline a parse failed. Drives fallback selection:
/// `JsonLd` failure falls through to `CatalogJson` / `CssSelector`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ParseStage {
    HtmlDocument,
    JsonLd,
    CatalogJson,
    CssSelector,
    ApiResponse,
    Price,
    Timestamp,
}

impl fmt::Display for ParseStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::HtmlDocument => "html_document",
            Self::JsonLd => "json_ld",
            Self::CatalogJson => "catalog_json",
            Self::CssSelector => "css_selector",
            Self::ApiResponse => "api_response",
            Self::Price => "price",
            Self::Timestamp => "timestamp",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ArbError {
    // ---- configuration / validation -------------------------------------------------
    #[error("config error in {context}: {message}")]
    Config { context: String, message: String },

    #[error("invalid {kind} identifier: {value:?}")]
    InvalidIdentifier { kind: &'static str, value: String },

    // ---- money ----------------------------------------------------------------------
    #[error("amount out of range: {dong} dong")]
    AmountOutOfRange { dong: i64 },

    #[error("rate out of range: {bps} bps")]
    RateOutOfRange { bps: i64 },

    #[error("arithmetic failure in {op}")]
    Arithmetic { op: &'static str },

    // ---- domain ---------------------------------------------------------------------
    #[error("unknown sku: {sku_id}")]
    UnknownSku { sku_id: SkuId },

    #[error("no usable buyback quote for {sku_id}")]
    NoBuybackQuote { sku_id: SkuId },

    // ---- connectors -----------------------------------------------------------------
    #[error("[{source_id}] transport error: {message}")]
    Transport {
        source_id: SourceId,
        message: String,
    },

    #[error("[{source_id}] timed out after {after_ms} ms")]
    Timeout { source_id: SourceId, after_ms: u64 },

    #[error("[{source_id}] http status {status}")]
    HttpStatus {
        source_id: SourceId,
        status: u16,
        /// Parsed `Retry-After`, if the server sent one. The scheduler must honour it.
        retry_after_secs: Option<u64>,
    },

    #[error("[{source_id}] local rate limit, retry in {retry_after_ms} ms")]
    RateLimited {
        source_id: SourceId,
        retry_after_ms: u64,
    },

    #[error("[{source_id}] circuit open until {retry_at}")]
    CircuitOpen {
        source_id: SourceId,
        retry_at: DateTime<Utc>,
    },

    #[error("[{source_id}] parse failure at {stage}: {message}")]
    Parse {
        source_id: SourceId,
        stage: ParseStage,
        message: String,
    },

    #[error("[{source_id}] authentication failed: {message}")]
    Auth {
        source_id: SourceId,
        message: String,
    },

    #[error("[{source_id}] upstream api error {code}: {message}")]
    UpstreamApi {
        source_id: SourceId,
        code: String,
        message: String,
    },

    #[error("[{source_id}] connector does not support this request: {message}")]
    Unsupported {
        source_id: SourceId,
        message: String,
    },

    // ---- infrastructure -------------------------------------------------------------
    #[error("storage error: {0}")]
    Storage(String),

    #[error("notify error: {0}")]
    Notify(String),

    #[error("shutting down")]
    Shutdown,

    #[error("internal invariant violated: {0}")]
    Internal(String),
}

impl ArbError {
    pub fn config(context: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Config {
            context: context.into(),
            message: message.into(),
        }
    }

    /// Whether retrying the *same* request later can succeed. Retry loops (`backon`)
    /// must gate on this; everything else is surfaced immediately.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transport { .. } | Self::Timeout { .. } | Self::RateLimited { .. } => true,
            Self::HttpStatus { status, .. } => {
                matches!(*status, 408 | 425 | 429 | 500 | 502 | 503 | 504)
            }
            _ => false,
        }
    }

    /// Whether this failure counts against a connector's circuit breaker.
    /// Local rate limiting and an already-open circuit are not upstream failures.
    pub fn trips_circuit(&self) -> bool {
        match self {
            Self::Transport { .. } | Self::Timeout { .. } | Self::Auth { .. } => true,
            Self::HttpStatus { status, .. } => *status == 429 || *status >= 500,
            _ => false,
        }
    }

    /// Stable low-cardinality label for metrics and log fields.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Config { .. } => "config",
            Self::InvalidIdentifier { .. } => "invalid_identifier",
            Self::AmountOutOfRange { .. } => "amount_out_of_range",
            Self::RateOutOfRange { .. } => "rate_out_of_range",
            Self::Arithmetic { .. } => "arithmetic",
            Self::UnknownSku { .. } => "unknown_sku",
            Self::NoBuybackQuote { .. } => "no_buyback_quote",
            Self::Transport { .. } => "transport",
            Self::Timeout { .. } => "timeout",
            Self::HttpStatus { .. } => "http_status",
            Self::RateLimited { .. } => "rate_limited",
            Self::CircuitOpen { .. } => "circuit_open",
            Self::Parse { .. } => "parse",
            Self::Auth { .. } => "auth",
            Self::UpstreamApi { .. } => "upstream_api",
            Self::Unsupported { .. } => "unsupported",
            Self::Storage(_) => "storage",
            Self::Notify(_) => "notify",
            Self::Shutdown => "shutdown",
            Self::Internal(_) => "internal",
        }
    }

    /// The connector this error is attributed to, if any.
    pub fn source_id(&self) -> Option<SourceId> {
        match self {
            Self::Transport { source_id, .. }
            | Self::Timeout { source_id, .. }
            | Self::HttpStatus { source_id, .. }
            | Self::RateLimited { source_id, .. }
            | Self::CircuitOpen { source_id, .. }
            | Self::Parse { source_id, .. }
            | Self::Auth { source_id, .. }
            | Self::UpstreamApi { source_id, .. }
            | Self::Unsupported { source_id, .. } => Some(*source_id),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_classification() {
        let s = SourceId::Tgdd;
        assert!(
            ArbError::HttpStatus {
                source_id: s,
                status: 503,
                retry_after_secs: None
            }
            .is_retryable()
        );
        assert!(
            !ArbError::HttpStatus {
                source_id: s,
                status: 404,
                retry_after_secs: None
            }
            .is_retryable()
        );
        assert!(
            !ArbError::Auth {
                source_id: s,
                message: String::new()
            }
            .is_retryable()
        );
        assert!(
            ArbError::Auth {
                source_id: s,
                message: String::new()
            }
            .trips_circuit()
        );
        assert!(
            !ArbError::RateLimited {
                source_id: s,
                retry_after_ms: 5
            }
            .trips_circuit()
        );
        assert_eq!(ArbError::Shutdown.source_id(), None);
    }
}
