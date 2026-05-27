use std::any::{TypeId, type_name};

use fabro_api::types::{
    CreateVariableRequest as ApiCreateVariableRequest,
    UpdateVariableRequest as ApiUpdateVariableRequest, Variable as ApiVariable,
    VariableListResponse as ApiVariableListResponse,
};
use fabro_types::{CreateVariableRequest, UpdateVariableRequest, Variable, VariableListResponse};
use serde_json::json;

#[test]
fn variable_api_types_reuse_canonical_types() {
    assert_same_type::<ApiVariable, Variable>();
    assert_same_type::<ApiVariableListResponse, VariableListResponse>();
    assert_same_type::<ApiCreateVariableRequest, CreateVariableRequest>();
    assert_same_type::<ApiUpdateVariableRequest, UpdateVariableRequest>();
}

#[test]
fn variable_round_trips_representative_json() {
    let value = json!({
        "name": "DEPLOY_ENV",
        "value": "production",
        "description": "Deployment target",
        "created_at": "2026-05-27T12:34:56Z",
        "updated_at": "2026-05-27T12:40:00Z"
    });

    let variable: Variable = serde_json::from_value(value.clone()).unwrap();

    assert_eq!(variable.name, "DEPLOY_ENV");
    assert_eq!(variable.value, "production");
    assert_eq!(variable.description.as_deref(), Some("Deployment target"));
    assert_eq!(serde_json::to_value(variable).unwrap(), value);
}

#[test]
fn variable_requests_round_trip_json() {
    let create = json!({
        "name": "EMPTY_ALLOWED",
        "value": "",
        "description": "Intentionally blank"
    });
    let parsed_create: CreateVariableRequest = serde_json::from_value(create.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed_create).unwrap(), create);

    let update = json!({
        "value": "updated"
    });
    let parsed_update: UpdateVariableRequest = serde_json::from_value(update.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed_update).unwrap(), update);
}

#[test]
fn variable_list_response_wraps_data() {
    let value = json!({
        "data": [{
            "name": "DEPLOY_ENV",
            "value": "production",
            "created_at": "2026-05-27T12:34:56Z",
            "updated_at": "2026-05-27T12:40:00Z"
        }]
    });

    let response: VariableListResponse = serde_json::from_value(value.clone()).unwrap();

    assert_eq!(response.data.len(), 1);
    assert_eq!(serde_json::to_value(response).unwrap(), value);
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
