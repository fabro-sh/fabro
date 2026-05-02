use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    Github,
    DevToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemActorKind {
    Engine,
    Watchdog,
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalLogFields {
    pub principal_kind:   &'static str,
    pub user_auth_method: Option<&'static str>,
    pub idp_issuer:       Option<String>,
    pub idp_subject:      Option<String>,
    pub login:            Option<String>,
    pub run_id:           Option<String>,
    pub delivery_id:      Option<String>,
    pub team_id:          Option<String>,
    pub user_id:          Option<String>,
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
    pub fn worker(run_id: RunId) -> Self {
        Self::Worker { run_id }
    }

    #[must_use]
    pub fn webhook(delivery_id: String) -> Self {
        Self::Webhook { delivery_id }
    }

    #[must_use]
    pub fn slack(team_id: String, user_id: String, user_name: Option<String>) -> Self {
        Self::Slack {
            team_id,
            user_id,
            user_name,
        }
    }

    #[must_use]
    pub fn agent(
        session_id: Option<String>,
        parent_session_id: Option<String>,
        model: Option<String>,
    ) -> Self {
        Self::Agent {
            session_id,
            parent_session_id,
            model,
        }
    }

    #[must_use]
    pub fn system(system_kind: SystemActorKind) -> Self {
        Self::System { system_kind }
    }

    #[must_use]
    pub fn anonymous() -> Self {
        Self::Anonymous
    }

    #[must_use]
    pub fn user_identity(&self) -> Option<&IdpIdentity> {
        match self {
            Self::User(user) => Some(&user.identity),
            _ => None,
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
            Self::System { system_kind } => format!("system:{system_kind:?}").to_lowercase(),
            Self::Anonymous => "anonymous".to_string(),
        }
    }

    #[must_use]
    pub fn log_fields(&self) -> PrincipalLogFields {
        match self {
            Self::User(user) => PrincipalLogFields {
                principal_kind:   "user",
                user_auth_method: Some(user.auth_method.as_str()),
                idp_issuer:       Some(user.identity.issuer().to_string()),
                idp_subject:      Some(user.identity.subject().to_string()),
                login:            Some(user.login.clone()),
                run_id:           None,
                delivery_id:      None,
                team_id:          None,
                user_id:          None,
            },
            Self::Worker { run_id } => PrincipalLogFields {
                principal_kind:   "worker",
                user_auth_method: None,
                idp_issuer:       None,
                idp_subject:      None,
                login:            None,
                run_id:           Some(run_id.to_string()),
                delivery_id:      None,
                team_id:          None,
                user_id:          None,
            },
            Self::Webhook { delivery_id } => PrincipalLogFields {
                principal_kind:   "webhook",
                user_auth_method: None,
                idp_issuer:       None,
                idp_subject:      None,
                login:            None,
                run_id:           None,
                delivery_id:      Some(delivery_id.clone()),
                team_id:          None,
                user_id:          None,
            },
            Self::Slack {
                team_id, user_id, ..
            } => PrincipalLogFields {
                principal_kind:   "slack",
                user_auth_method: None,
                idp_issuer:       None,
                idp_subject:      None,
                login:            None,
                run_id:           None,
                delivery_id:      None,
                team_id:          Some(team_id.clone()),
                user_id:          Some(user_id.clone()),
            },
            Self::Agent { .. } => PrincipalLogFields {
                principal_kind:   "agent",
                user_auth_method: None,
                idp_issuer:       None,
                idp_subject:      None,
                login:            None,
                run_id:           None,
                delivery_id:      None,
                team_id:          None,
                user_id:          None,
            },
            Self::System { .. } => PrincipalLogFields {
                principal_kind:   "system",
                user_auth_method: None,
                idp_issuer:       None,
                idp_subject:      None,
                login:            None,
                run_id:           None,
                delivery_id:      None,
                team_id:          None,
                user_id:          None,
            },
            Self::Anonymous => PrincipalLogFields {
                principal_kind:   "anonymous",
                user_auth_method: None,
                idp_issuer:       None,
                idp_subject:      None,
                login:            None,
                run_id:           None,
                delivery_id:      None,
                team_id:          None,
                user_id:          None,
            },
        }
    }
}

impl AuthMethod {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::DevToken => "dev_token",
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{AuthMethod, Principal, SystemActorKind};
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
        let principal = Principal::system(SystemActorKind::Watchdog);

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
            Principal::worker(fixtures::RUN_1),
            Principal::webhook("delivery-1".to_string()),
            Principal::slack("T1".to_string(), "U1".to_string(), Some("ada".to_string())),
            Principal::agent(
                Some("session".to_string()),
                Some("parent".to_string()),
                Some("gpt".to_string()),
            ),
            Principal::system(SystemActorKind::Engine),
            Principal::anonymous(),
        ];

        for principal in variants {
            let value = serde_json::to_value(&principal).unwrap();
            let parsed: Principal = serde_json::from_value(value).unwrap();
            assert_eq!(parsed, principal);
        }
    }

    #[test]
    fn projects_log_fields() {
        let user = Principal::user(identity(), "octocat".to_string(), AuthMethod::DevToken);
        let fields = user.log_fields();

        assert_eq!(fields.principal_kind, "user");
        assert_eq!(fields.user_auth_method, Some("dev_token"));
        assert_eq!(fields.idp_issuer.as_deref(), Some("https://github.com"));
        assert_eq!(fields.idp_subject.as_deref(), Some("12345"));
        assert_eq!(fields.login.as_deref(), Some("octocat"));

        let worker = Principal::worker(fixtures::RUN_1);
        assert_eq!(worker.log_fields().principal_kind, "worker");
        assert_eq!(
            worker.log_fields().run_id,
            Some(fixtures::RUN_1.to_string())
        );
    }
}
