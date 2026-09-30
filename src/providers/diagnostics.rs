//! Closed diagnostic vocabulary: no provider-controlled text crosses this boundary.
use crate::domain::*;

pub(super) fn allowed(reason: &str) -> bool {
    matches!(
        reason,
        "incomplete_max_output_tokens"
            | "incomplete"
            | "failed"
            | "cancelled"
            | "refusal"
            | "tool_output"
            | "unknown_output_item"
            | "assistant_message_count"
            | "message_incomplete"
            | "output_text_missing"
            | "output_json_invalid"
            | "schema_invalid"
            | "usage_invalid"
            | "model_invalid"
            | "response_envelope_invalid"
            | "response_headers_invalid"
            | "response_encoding_unsupported"
            | "response_status_invalid"
            | "response_bound"
            | "response_length_invalid"
            | "wire_contract_violation"
            | "incomplete_response"
            | "provider_output_invalid"
            | "uncertain_retry_requires_opt_in"
    ) || reason.strip_prefix("http_").is_some_and(|s| {
        s.len() == 3
            && s.bytes().all(|c| c.is_ascii_digit())
            && s.parse::<u16>()
                .is_ok_and(|status| (100..600).contains(&status))
    })
}
pub(super) fn reason(error: &WikiError) -> Option<&str> {
    error
        .details
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .filter(|r| allowed(r))
}
pub(super) fn error(code: ErrorCode, reason: &str) -> WikiError {
    let mut error = WikiError::new(code, "bounded provider operation failed");
    if allowed(reason) {
        error.details = serde_json::json!({"reason": reason});
    }
    error
}
pub(super) fn decoded(error: WikiError) -> WikiError {
    // Legacy decoders use fixed local messages. Map only exact literals, never
    // copy arbitrary messages, field names or provider body fragments.
    let reason = reason(&error).unwrap_or(match error.message.as_str() {
        "embedding usage malformed" | "generation usage malformed" => "usage_invalid",
        "invalid or excessive JSON document" | "trailing JSON content" => "output_json_invalid",
        "incomplete provider response" => "incomplete_response",
        "generation output violates local schema" => "schema_invalid",
        _ => "provider_output_invalid",
    });
    self::error(ErrorCode::ProviderResponse, reason)
}
