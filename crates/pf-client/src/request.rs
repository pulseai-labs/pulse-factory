//! The one function that builds a `/v1/verbs/<verb>` request.
//!
//! A device may call only the verbs of [`pf_api::ALLOWLIST`] (ADR-0012, `client-api-auth` admission).
//! This function is where the client enforces it: a verb that is not on the list is a `bad_request`
//! here, before any connection.

use serde_json::Value;

use crate::error::{ClientError, ErrorKind};

/// A request that needs only a method, a path and a JSON body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiRequest {
    pub method: &'static str,
    pub path: String,
    pub body: Option<Vec<u8>>,
}

/// `POST /v1/verbs/<verb>` with the arguments as the JSON body.
pub fn verb_request(verb: &str, args: &Value) -> Result<ApiRequest, ClientError> {
    if !pf_api::ALLOWLIST.contains(&verb) {
        return Err(ClientError::new(
            ErrorKind::BadRequest,
            "that verb is not one a device may call",
        ));
    }
    if !args.is_object() {
        return Err(ClientError::new(
            ErrorKind::BadRequest,
            "the verb arguments must be a JSON object",
        ));
    }
    let body = serde_json::to_vec(args).map_err(|_| {
        ClientError::new(
            ErrorKind::BadRequest,
            "the verb arguments cannot be written",
        )
    })?;
    Ok(ApiRequest {
        method: "POST",
        path: format!("/v1/verbs/{verb}"),
        body: Some(body),
    })
}
