use serde::{Deserialize, Serialize};
use strum::{Display, IntoStaticStr};

use crate::{IdpIdentity, RunId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPrincipal {
    pub identity:    IdpIdentity,
    pub login:       String,
    pub auth_method: AuthMethod,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Principal {
    User(UserPrincipal),
    Worker {
        run_id: RunId,
    },
    Webhook {
        delivery_id: String,
    },
    Slack {
        team_id:   String,
        user_id:   String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_name: Option<String>,
    },
    Agent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id:        Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model:             Option<String>,
    },
    System {
        system_kind: SystemActorKind,
    },
    Anonymous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, IntoStaticStr)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum AuthMethod {
    Github,
    DevToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum SystemActorKind {
    Engine,
    Watchdog,
    Timeout,
}

impl Principal {
    #[must_use]
    pub fn user(identity: IdpIdentity, login: String, auth_method: AuthMethod) -> Self {
        Self::User(UserPrincipal {
            identity,
            login,
            auth_method,
        })
    }

    #[must_use]
    pub fn user_identity(&self) -> Option<&IdpIdentity> {
        match self {
            Self::User(user) => Some(&user.identity),
            _ => None,
        }
    }

    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::User(_) => "user",
            Self::Worker { .. } => "worker",
            Self::Webhook { .. } => "webhook",
            Self::Slack { .. } => "slack",
            Self::Agent { .. } => "agent",
            Self::System { .. } => "system",
            Self::Anonymous => "anonymous",
        }
    }

    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::User(user) => user.login.clone(),
            Self::Worker { run_id } => run_id.to_string(),
            Self::Webhook { delivery_id } => delivery_id.clone(),
            Self::Slack {
                user_name: Some(user_name),
                ..
            } => user_name.clone(),
            Self::Slack {
                team_id, user_id, ..
            } => format!("{team_id}:{user_id}"),
            Self::Agent {
                model: Some(model), ..
            } => model.clone(),
            Self::Agent {
                session_id: Some(session_id),
                ..
            } => session_id.clone(),
            Self::Agent { .. } => "agent".to_string(),
            Self::System { system_kind } => format!("system:{system_kind}"),
            Self::Anonymous => "anonymous".to_string(),
        }
    }
}

impl AuthMethod {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{AuthMethod, Principal, SystemActorKind, UserPrincipal};
    use crate::{IdpIdentity, fixtures};

    fn identity() -> IdpIdentity {
        IdpIdentity::new("https://github.com", "12345").unwrap()
    }

    #[test]
    fn user_principal_serializes_flat_with_identity() {
        let principal = Principal::user(identity(), "octocat".to_string(), AuthMethod::Github);

        assert_eq!(
            serde_json::to_value(&principal).unwrap(),
            json!({
                "kind": "user",
                "identity": {
                    "issuer": "https://github.com",
                    "subject": "12345"
                },
                "login": "octocat",
                "auth_method": "github"
            })
        );
    }

    #[test]
    fn system_principal_uses_system_kind_field() {
        let principal = Principal::System {
            system_kind: SystemActorKind::Watchdog,
        };

        assert_eq!(
            serde_json::to_value(&principal).unwrap(),
            json!({
                "kind": "system",
                "system_kind": "watchdog"
            })
        );
    }

    #[test]
    fn round_trips_all_variants() {
        let variants = [
            Principal::user(identity(), "octocat".to_string(), AuthMethod::Github),
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
                session_id:        Some("session".to_string()),
                parent_session_id: Some("parent".to_string()),
                model:             Some("gpt".to_string()),
            },
            Principal::System {
                system_kind: SystemActorKind::Engine,
            },
            Principal::Anonymous,
        ];

        for principal in variants {
            let value = serde_json::to_value(&principal).unwrap();
            let parsed: Principal = serde_json::from_value(value).unwrap();
            assert_eq!(parsed, principal);
        }
    }

    #[test]
    fn auth_method_as_str_matches_serde() {
        assert_eq!(AuthMethod::Github.as_str(), "github");
        assert_eq!(AuthMethod::DevToken.as_str(), "dev_token");
    }

    #[test]
    fn system_actor_kind_displays_snake_case() {
        assert_eq!(SystemActorKind::Engine.to_string(), "engine");
        assert_eq!(SystemActorKind::Watchdog.to_string(), "watchdog");
        assert_eq!(SystemActorKind::Timeout.to_string(), "timeout");
    }

    #[test]
    fn user_principal_kind_is_user() {
        let principal = Principal::User(UserPrincipal {
            identity:    identity(),
            login:       "octocat".to_string(),
            auth_method: AuthMethod::Github,
        });
        assert_eq!(principal.kind(), "user");
    }
}
