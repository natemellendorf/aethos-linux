use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: &str = "0.1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloEnvelope {
    pub version: String,
    pub message_type: String,
    pub platform: String,
    pub client: String,
    pub wayfair_id: String,
}

impl HelloEnvelope {
    pub fn new(wayfair_id: impl Into<String>) -> Self {
        Self {
            version: PROTOCOL_VERSION.to_string(),
            message_type: "hello".to_string(),
            platform: "linux".to_string(),
            client: "aethos-linux".to_string(),
            wayfair_id: wayfair_id.into(),
        }
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

#[cfg(test)]
mod tests {
    use super::HelloEnvelope;

    #[test]
    fn hello_envelope_serializes() {
        let envelope = HelloEnvelope::new("wf-123");
        let serialized = envelope.to_json().expect("serialize envelope");

        assert!(serialized.contains("\"message_type\":\"hello\""));
        assert!(serialized.contains("\"wayfair_id\":\"wf-123\""));
    }
}
