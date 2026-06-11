use std::any::{TypeId, type_name};

use fabro_api::types::CostSource as ApiCostSource;
use fabro_model::CostSource;
use serde_json::json;

#[test]
fn cost_source_reuses_canonical_type() {
    assert_same_type::<ApiCostSource, CostSource>();
}

#[test]
fn cost_source_json_matches_openapi_shape() {
    assert_eq!(
        serde_json::to_value(CostSource::Authoritative).unwrap(),
        json!("authoritative")
    );
    assert_eq!(
        serde_json::to_value(CostSource::Estimated).unwrap(),
        json!("estimated")
    );

    assert_eq!(
        serde_json::from_value::<ApiCostSource>(json!("estimated")).unwrap(),
        CostSource::Estimated
    );
}

fn assert_same_type<T: 'static, U: 'static>() {
    assert_eq!(
        TypeId::of::<T>(),
        TypeId::of::<U>(),
        "{} should be the same type as {}",
        type_name::<T>(),
        type_name::<U>()
    );
}
