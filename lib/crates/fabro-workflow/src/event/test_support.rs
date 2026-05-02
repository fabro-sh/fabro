use ::fabro_types::{AuthMethod, IdpIdentity, Principal};

pub(crate) fn user_principal(login: &str) -> Principal {
    Principal::user(
        IdpIdentity::new("https://github.com", "12345").unwrap(),
        login.to_string(),
        AuthMethod::Github,
    )
}
