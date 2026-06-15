#[derive(Clone)]
pub struct GitLabTokenSource {
    token: String,
}

impl GitLabTokenSource {
    pub fn new_static(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }

    pub fn token(&self) -> anyhow::Result<String> {
        Ok(self.token.clone())
    }
}
