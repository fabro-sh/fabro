use std::any::{TypeId, type_name};

use fabro_api::types::Model as ApiModel;
use fabro_model::{
    Model, ModelCosts, ModelFeatures, ModelLimits, ProviderId, ReasoningEffortFeature,
};
use serde_json::json;

#[test]
fn provider_id_reuses_canonical_model_field_type() {
    assert_same_type::<ApiModel, Model>();
}

#[test]
fn provider_id_json_matches_openapi_shape_through_model() {
    assert_eq!(
        serde_json::to_value(ProviderId::anthropic()).unwrap(),
        json!("anthropic")
    );
    assert_eq!(
        serde_json::to_value(ProviderId::openai()).unwrap(),
        json!("openai")
    );

    let model = Model {
        id:                   "venice-custom".to_string(),
        provider:             ProviderId::new("venice"),
        family:               "venice".to_string(),
        display_name:         "Venice Custom".to_string(),
        limits:               ModelLimits {
            context_window: 128_000,
            max_output:     None,
        },
        training:             None,
        knowledge_cutoff:     None,
        features:             ModelFeatures {
            tools:            false,
            vision:           false,
            reasoning:        false,
            reasoning_effort: ReasoningEffortFeature::None,
            prompt_cache:     false,
            sampling_params:  true,
        },
        costs:                ModelCosts {
            input_cost_per_mtok:       None,
            output_cost_per_mtok:      None,
            cache_input_cost_per_mtok: None,
        },
        estimated_output_tps: None,
        aliases:              Vec::new(),
        default:              false,
        small_default:        false,
        configured:           true,
    };

    let json = serde_json::to_value(&model).unwrap();
    assert_eq!(json["provider"], "venice");
    let round_trip: ApiModel = serde_json::from_value(json).unwrap();
    assert_eq!(round_trip.provider, ProviderId::new("venice"));
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
