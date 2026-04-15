use fabro_model::Provider;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthContextRequest {
    ApiKey {
        provider: Provider,
        env_var_names: Vec<String>,
    },
    DeviceCode {
        user_code: String,
        verification_uri: String,
        expires_in: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthContextResponse {
    ApiKey { key: String },
    DeviceCodeConfirmed,
}
