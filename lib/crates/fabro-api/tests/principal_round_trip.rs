use std::any::{TypeId, type_name};

use fabro_api::types::{Principal as ApiPrincipal, RunProvenance as ApiRunProvenance};
use fabro_types::{
    AuthMethod, IdpIdentity, Principal, RunClientProvenance, RunProvenance, RunServerProvenance,
    SystemActorKind, fixtures,
};
use serde_json::{Value, json};

const OCTOCAT_AVATAR_URL: &str = "https://example.com/octocat.png";

fn octocat_identity() -> IdpIdentity {
    IdpIdentity::new("https://github.com", "12345").unwrap()
}

fn octocat_user() -> Principal {
    Principal::user(
        octocat_identity(),
        "octocat".to_string(),
        AuthMethod::Github,
    )
}

fn octocat_user_with_avatar() -> Principal {
    Principal::user_with_avatar(
        octocat_identity(),
        "octocat".to_string(),
        AuthMethod::Github,
        Some(OCTOCAT_AVATAR_URL.to_string()),
    )
}

fn octocat_user_json() -> Value {
    json!({
        "kind": "user",
        "identity": {
            "issuer": "https://github.com",
            "subject": "12345"
        },
        "login": "octocat",
        "auth_method": "github"
    })
}

fn octocat_user_with_avatar_json() -> Value {
    let mut value = octocat_user_json();
    value["avatar_url"] = json!(OCTOCAT_AVATAR_URL);
    value
}

#[test]
fn principal_reuses_canonical_type() {
    assert_same_type::<ApiPrincipal, Principal>();
}

#[test]
fn run_provenance_reuses_canonical_type() {
    assert_same_type::<ApiRunProvenance, RunProvenance>();
}

#[test]
fn principal_round_trips_representative_json() {
    let value = octocat_user_json();

    let principal: Principal = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(principal, octocat_user());
    assert_eq!(serde_json::to_value(principal).unwrap(), value);
}

#[test]
fn principal_user_with_avatar_round_trips_through_api_type() {
    let value = octocat_user_with_avatar_json();

    let principal: Principal = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(principal, octocat_user_with_avatar());

    let api_principal: ApiPrincipal = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(api_principal, principal);
    assert_eq!(serde_json::to_value(api_principal).unwrap(), value);
}

#[test]
fn principal_system_uses_system_kind_field() {
    let principal = Principal::System {
        system_kind: SystemActorKind::Watchdog,
    };

    assert_eq!(
        serde_json::to_value(principal).unwrap(),
        json!({
            "kind": "system",
            "system_kind": "watchdog"
        })
    );
}

#[test]
fn principal_round_trips_every_variant_through_api_type() {
    let variants = vec![
        octocat_user(),
        octocat_user_with_avatar(),
        Principal::Worker {
            run_id: fixtures::RUN_1,
        },
        Principal::Webhook {
            delivery_id: "delivery-1".to_string(),
        },
        Principal::Slack {
            team_id:   "T1".to_string(),
            user_id:   "U1".to_string(),
            user_name: Some("ada".to_string()),
        },
        Principal::Agent {
            session_id:        Some("ses_agent".to_string()),
            parent_session_id: Some("ses_parent".to_string()),
            model:             Some("gpt-5.4".to_string()),
        },
        Principal::System {
            system_kind: SystemActorKind::Watchdog,
        },
        Principal::Anonymous,
    ];

    for principal in variants {
        let json = serde_json::to_value(&principal).unwrap();
        let api_principal: ApiPrincipal = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(api_principal, principal);
        assert_eq!(serde_json::to_value(api_principal).unwrap(), json);
    }
}

#[test]
fn run_provenance_subject_round_trips_as_principal() {
    let provenance = RunProvenance {
        server:  Some(RunServerProvenance {
            version: "0.1.0".to_string(),
        }),
        client:  Some(RunClientProvenance {
            user_agent: Some("fabro-cli/0.1.0".to_string()),
            name:       Some("fabro-cli".to_string()),
            version:    Some("0.1.0".to_string()),
        }),
        subject: Some(Principal::Worker {
            run_id: fixtures::RUN_1,
        }),
    };
    let json = serde_json::to_value(&provenance).unwrap();

    let api_provenance: ApiRunProvenance = serde_json::from_value(json.clone()).unwrap();

    assert_eq!(api_provenance, provenance);
    assert_eq!(serde_json::to_value(api_provenance).unwrap(), json);
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
