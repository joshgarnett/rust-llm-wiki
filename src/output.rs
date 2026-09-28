//! Versioned machine output. Presentation never substitutes for durable state.
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub struct Envelope {
    pub schema_version: &'static str,
    pub command: String,
    pub ok: bool,
    pub data: Value,
    pub meta: Metadata,
    pub warnings: Vec<String>,
    pub error: Option<ErrorOutput>,
}

#[derive(Debug, Default, Serialize)]
pub struct Metadata {
    pub wiki_id: Option<String>,
    pub index_generation: Option<u64>,
    pub freshness: Option<String>,
    pub verified_at: Option<String>,
    pub partial: bool,
    pub network_used: bool,
}

#[derive(Debug, Serialize)]
pub struct ErrorOutput {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub hint: Option<String>,
    pub details: Value,
}

impl Envelope {
    pub fn success(command: &str, data: Value) -> Self {
        Self {
            schema_version: "1",
            command: command.into(),
            ok: true,
            data,
            meta: Metadata::default(),
            warnings: Vec::new(),
            error: None,
        }
    }

    pub fn failure(command: &str, code: &str, message: String) -> Self {
        Self {
            schema_version: "1",
            command: command.into(),
            ok: false,
            data: Value::Null,
            meta: Metadata::default(),
            warnings: Vec::new(),
            error: Some(ErrorOutput {
                code: code.into(),
                message,
                retryable: false,
                hint: None,
                details: Value::Null,
            }),
        }
    }
}
