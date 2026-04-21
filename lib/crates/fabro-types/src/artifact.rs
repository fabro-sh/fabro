use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactUpload {
    pub path:           String,
    pub mime:           String,
    pub content_md5:    String,
    pub content_sha256: String,
    pub bytes:          u64,
}

#[cfg(test)]
mod tests {
    use super::ArtifactUpload;

    #[test]
    fn round_trips_through_serde_json() {
        let artifact = ArtifactUpload {
            path:           "artifacts/log.txt".to_string(),
            mime:           "text/plain".to_string(),
            content_md5:    "md5".to_string(),
            content_sha256: "sha256".to_string(),
            bytes:          42,
        };

        let value = serde_json::to_value(&artifact).unwrap();
        let parsed: ArtifactUpload = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, artifact);
    }
}
