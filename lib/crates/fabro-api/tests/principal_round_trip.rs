use std::any::{TypeId, type_name};

use fabro_api::types::{Principal as ApiPrincipal, RunProvenance as ApiRunProvenance};
use fabro_types::{
    AuthMethod, IdpIdentity, Principal, RunClientProvenance, RunProvenance, RunServerProvenance,
    SystemActorKind, fixtures,
};
use serde_json::json;

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

#[test]
fn principal_round_trips_every_variant_through_api_type() {
    let variants = vec![
        Principal::user(
            IdpIdentity::new("https://github.com", "12345").unwrap(),
            "octocat".to_string(),
            AuthMethod::Github,
        ),
        Principal::worker(fixtures::RUN_1),
        Principal::webhook("delivery-1".to_string()),
        Principal::slack("T1".to_string(), "U1".to_string(), Some("ada".to_string())),
        Principal::agent(
            Some("ses_agent".to_string()),
            Some("ses_parent".to_string()),
            Some("gpt-5.4".to_string()),
        ),
        Principal::system(SystemActorKind::Watchdog),
        Principal::anonymous(),
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
        subject: Some(Principal::worker(fixtures::RUN_1)),
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
