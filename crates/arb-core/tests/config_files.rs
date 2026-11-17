//! The shipped example configs must deserialize into the contract types and pass
//! validation. This is the Wave 0 gate that proves the TOML schema and `arb-core` agree.

use std::path::Path;

use arb_core::{EngineConfig, SkuCatalog, VoucherBook};

fn load<T: serde::de::DeserializeOwned>(name: &str) -> T {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../config")
        .join(name);
    let text = std::fs::read_to_string(&path).unwrap();
    toml::from_str(&text).unwrap()
}

#[test]
fn skus_toml_is_valid() {
    let catalog: SkuCatalog = load("skus.toml");
    catalog.validate().unwrap();
    assert!(catalog.enabled().count() >= 1);
}

#[test]
fn vouchers_toml_is_valid_and_references_known_skus() {
    let catalog: SkuCatalog = load("skus.toml");
    let book: VoucherBook = load("vouchers_11_11.toml");
    book.validate().unwrap();
    for rule in &book.voucher {
        for sku in &rule.eligible_skus {
            assert!(
                catalog.get(sku).is_some(),
                "voucher {} references unknown sku {sku}",
                rule.id
            );
        }
    }
}

#[test]
fn engine_toml_is_valid() {
    let config: EngineConfig = load("engine.toml");
    config.validate().unwrap();
    assert_eq!(config.schedule.windows.len(), 3);
}
