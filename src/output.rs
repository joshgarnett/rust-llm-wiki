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
    /// Versioned target binding and non-secret effective preference origins.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection: Option<Value>,
    pub index_generation: Option<u64>,
    /// Complete command-local observation, distinct from a published index.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_view: Option<Value>,
    pub freshness: Option<String>,
    pub verified_at: Option<String>,
    pub partial: bool,
    pub network_used: bool,
    /// Present only for an explicitly activated experimental context route.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_strategy: Option<String>,
    /// Joined maintenance work, when this command submitted any worker jobs.
    /// Service durations can overlap and are not command elapsed time.
    /// In-flight bytes are admitted logical workspace, not actual process RSS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maintenance: Option<Value>,
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
            schema_version: "2",
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
            schema_version: "2",
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
