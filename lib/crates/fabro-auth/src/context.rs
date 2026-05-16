use fabro_model::ProviderId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthContextRequest {
    ApiKey {
        provider_id:   ProviderId,
        display_name:  String,
        env_var_names: Vec<String>,
        api_key_url:   Option<String>,
    },
    DeviceCode {
        user_code:        String,
        verification_uri: String,
        expires_in:       u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthContextResponse {
    ApiKey { key: String },
    DeviceCodeConfirmed,
}
