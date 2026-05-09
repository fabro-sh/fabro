use fabro_api::types::SubmitAnswerRequest;
use serde_json::json;

#[test]
fn typed_answer_variants_round_trip_through_json() {
    let cases = [
        json!({ "kind": "yes" }),
        json!({ "kind": "no" }),
        json!({ "kind": "selected", "option_key": "approve" }),
        json!({ "kind": "multi_selected", "option_keys": ["approve", "notify"] }),
        json!({ "kind": "text", "text": "Looks good to me." }),
    ];

    for payload in cases {
        let request: SubmitAnswerRequest = serde_json::from_value(payload.clone()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), payload);
    }
}

#[test]
fn legacy_answer_fields_are_not_part_of_the_wire_contract() {
    let result = serde_json::from_value::<SubmitAnswerRequest>(json!({ "value": "yes" }));

    assert!(result.is_err());
}
