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
            | "credential_file_unavailable_or_invalid"
            | "credential_environment_missing"
            | "credential_environment_unavailable"
            | "credential_token_invalid"
            | "credential_helper_failed"
            | "response_byte_budget_exceeded"
            | "request_byte_budget_exceeded"
            | "request_budget_exceeded"
            | "lifetime_budget_exceeded"
            | "run_deadline_expired"
            | "attempt_deadline_exceeded"
            | "unproven_token_cost_bound"
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
    let (message, next_action) = match reason {
        "credential_file_unavailable_or_invalid" => (
            "credential file is unavailable or fails protection or format checks",
            Some(
                "Check that the configured credential file is a readable, bounded regular file; on Unix it must have no group or other permissions (for example mode 0600). One terminal LF or CRLF is accepted. Do not place credentials in the vault.",
            ),
        ),
        "credential_environment_missing" | "credential_environment_unavailable" => (
            "configured credential environment variable is missing or unavailable",
            Some(
                "Set the credential variable named by the trusted provider configuration in the invoking process environment, then retry explicitly.",
            ),
        ),
        "credential_token_invalid" => (
            "credential token is empty, invalid UTF-8, contains control characters, or exceeds its bound",
            Some(
                "Check the configured credential source without printing its contents. Environment values must not contain a terminal newline.",
            ),
        ),
        "credential_helper_failed" => (
            "configured credential helper failed",
            Some(
                "Check the trusted helper configuration and its bounded output contract; helper output is not included in diagnostics.",
            ),
        ),
        "response_byte_budget_exceeded" => (
            "response-byte reservation exceeds the remaining lifetime budget",
            Some(
                "Embedding attempts reserve 8388608 response bytes (8 MiB) each; generation attempts reserve 1048576 bytes (1 MiB). Settled usage and outstanding reservations also count. Inspect jobs status before explicitly revising a budget; unknown charges remain reserved.",
            ),
        ),
        "request_byte_budget_exceeded" => (
            "request-byte reservation exceeds the remaining lifetime budget",
            Some(
                "Inspect jobs status and the requested input size before explicitly revising the request-byte budget.",
            ),
        ),
        "request_budget_exceeded" | "lifetime_budget_exceeded" => (
            "settled usage plus outstanding and proposed reservations exceeds the lifetime budget",
            Some(
                "Inspect jobs status for cumulative usage and outstanding reservations before explicitly amending retained limits.",
            ),
        ),
        "run_deadline_expired" | "attempt_deadline_exceeded" => (
            "remaining run deadline cannot admit the configured attempt timeout",
            Some(
                "Inspect jobs status; an explicit deadline amendment must leave room for the complete configured provider timeout.",
            ),
        ),
        "unproven_token_cost_bound" => (
            "the configured adapter and model cannot prove the requested hard token or cost bound",
            Some(
                "Inspect the provider bound contract. Request and byte limits remain available; do not silently remove or raise an authorized token or cost ceiling.",
            ),
        ),
        _ => ("bounded provider operation failed", None),
    };
    let mut error = WikiError::new(code, message);
    if allowed(reason) {
        error.details = serde_json::json!({"reason": reason});
        if let Some(next_action) = next_action {
            error.details["next_action"] = next_action.into();
        }
    }
    error
}
/// Exact locally generated literals only; arbitrary error messages and details
/// can contain provider/helper data and must never be copied to public output.
pub(super) fn local(error: &WikiError) -> Option<&'static str> {
    match (error.code, error.message.as_str()) {
        (ErrorCode::ProviderAuth, "credential file unavailable or invalid") => {
            Some("credential_file_unavailable_or_invalid")
        }
        (ErrorCode::ProviderAuth, "credential environment source missing") => {
            Some("credential_environment_missing")
        }
        (ErrorCode::ProviderAuth, "credential environment source unavailable") => {
            Some("credential_environment_unavailable")
        }
        (
            ErrorCode::ProviderAuth,
            "credential token empty, invalid, or oversized"
            | "credential encoding invalid"
            | "credential input exceeds byte ceiling",
        ) => Some("credential_token_invalid"),
        (ErrorCode::ProviderAuth, "credential helper failed") => Some("credential_helper_failed"),
        (ErrorCode::BudgetExceeded, "lifetime response-byte reservation exceeds run budget") => {
            Some("response_byte_budget_exceeded")
        }
        (
            ErrorCode::BudgetExceeded,
            "lifetime request-byte reservation exceeds run budget" | "request body exceeds cap",
        ) => Some("request_byte_budget_exceeded"),
        (ErrorCode::BudgetExceeded, "lifetime request reservation exceeds run budget") => {
            Some("request_budget_exceeded")
        }
        (
            ErrorCode::BudgetExceeded,
            "lifetime settled plus outstanding plus proposed exceeds run budget",
        ) => Some("lifetime_budget_exceeded"),
        (ErrorCode::BudgetExceeded, "run deadline expired") => Some("run_deadline_expired"),
        (ErrorCode::BudgetExceeded, "attempt timeout exceeds remaining deadline") => {
            Some("attempt_deadline_exceeded")
        }
        (
            ErrorCode::CapabilityUnavailable,
            "hard cost/token ceiling requires proven complete upper bounds"
            | "configured input token ceiling has no sufficient proof",
        ) => Some("unproven_token_cost_bound"),
        _ => None,
    }
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
