use crate::Principal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteeringMessage {
    pub text:  String,
    pub actor: Option<Principal>,
}

impl SteeringMessage {
    #[must_use]
    pub fn new(text: impl Into<String>, actor: Option<Principal>) -> Self {
        Self {
            text: text.into(),
            actor,
        }
    }
}
