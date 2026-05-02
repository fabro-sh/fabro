use std::any::{TypeId, type_name};

use fabro_api::types::Principal as ApiPrincipal;
use fabro_types::{AuthMethod, IdpIdentity, Principal, SystemActorKind};
use serde_json::json;

#[test]
fn principal_reuses_canonical_type() {
    assert_same_type::<ApiPrincipal, Principal>();
}

#[test]
fn principal_round_trips_representative_json() {
    let value = json!({
        "kind": "user",
        "identity": {
            "issuer": "https://github.com",
            "subject": "12345"
        },
        "login": "octocat",
        "auth_method": "github"
    });

    let principal: Principal = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        principal,
        Principal::user(
            IdpIdentity::new("https://github.com", "12345").unwrap(),
            "octocat".to_string(),
            AuthMethod::Github,
        )
    );
    assert_eq!(serde_json::to_value(principal).unwrap(), value);
}

#[test]
fn principal_system_uses_system_kind_field() {
    let principal = Principal::system(SystemActorKind::Watchdog);

    assert_eq!(
        serde_json::to_value(principal).unwrap(),
        json!({
            "kind": "system",
            "system_kind": "watchdog"
        })
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
