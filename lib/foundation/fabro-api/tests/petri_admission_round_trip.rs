use std::any::{TypeId, type_name};

use fabro_api::types::{PetriAdmission as ApiPetriAdmission, PetriGraphRef as ApiPetriGraphRef};
use fabro_types::{PetriAdmission, PetriGraphRef};
use serde_json::json;

#[test]
fn the_admission_reuses_canonical_types() {
    assert_same_type::<ApiPetriAdmission, PetriAdmission>();
    assert_same_type::<ApiPetriGraphRef, PetriGraphRef>();
}

#[test]
fn the_admission_round_trips_with_its_children() {
    let value = json!({
        "graph": {
            "blob": "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
            "digest": "sha256:root"
        },
        "children": [
            {
                "blob": "3cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
                "digest": "sha256:child"
            }
        ]
    });
    let admission: PetriAdmission = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(admission.graph.digest, "sha256:root");
    assert_eq!(admission.children.len(), 1);
    assert_eq!(serde_json::to_value(&admission).unwrap(), value);
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
