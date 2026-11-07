//! The transport: the six operations of the UI's interface (`docs/contracts/ui-transport.md`).
//!
//! `paired`, `pair`, `status`, `project_list`, `project_add` and `forget`, with the names of the
//! interface in snake case. The device token goes from `POST /v1/pair` into the [`DeviceStore`] and
//! no further: no method returns it, and no error holds it.

use std::sync::Arc;

use pf_api::code::PairingCode;
use pf_api::types::{
    ApiStatus, DeviceRecord, PairRequest, PairResult, ProjectAddArgs, ProjectChanged, ProjectList,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ClientError, ErrorKind, Route, error_from_status};
use crate::http::{Timeouts, pinned_call};
use crate::pin::parse_pin;
use crate::request::{ApiRequest, verb_request};
use crate::store::{DeviceStore, Paired};

/// What `paired` and `pair` return: the server's address and this device's record. Never the token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedServer {
    pub address: String,
    pub device: DeviceRecord,
}

/// The six operations over a device store.
pub struct Transport<S: DeviceStore> {
    store: Arc<S>,
    timeouts: Timeouts,
}

impl<S: DeviceStore> Clone for Transport<S> {
    fn clone(&self) -> Self {
        Transport {
            store: self.store.clone(),
            timeouts: self.timeouts,
        }
    }
}

fn not_documented() -> ClientError {
    ClientError::new(
        ErrorKind::Server,
        "the server's answer is not the documented one",
    )
}

fn parse_answer<T: DeserializeOwned>(value: Value) -> Result<T, ClientError> {
    serde_json::from_value(value).map_err(|_| not_documented())
}

impl<S: DeviceStore> Transport<S> {
    /// Run one call of the device store on the blocking pool. The store is synchronous and does file
    /// work (the Keychain store adds Keychain calls), so it never runs on the async thread.
    async fn on_store<T, F>(&self, work: F) -> Result<T, ClientError>
    where
        T: Send + 'static,
        F: FnOnce(&S) -> Result<T, ClientError> + Send + 'static,
    {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || work(&store))
            .await
            .map_err(|_| {
                ClientError::new(ErrorKind::Server, "the device store call did not finish")
            })?
    }

    pub fn new(store: Arc<S>) -> Transport<S> {
        Transport {
            store,
            timeouts: Timeouts::default(),
        }
    }

    /// The same transport with other time limits, for tests.
    pub fn with_timeouts(mut self, timeouts: Timeouts) -> Transport<S> {
        self.timeouts = timeouts;
        self
    }

    /// The paired server, or `None`. No network call.
    pub async fn paired(&self) -> Result<Option<PairedServer>, ClientError> {
        Ok(self
            .on_store(|store| store.load())
            .await?
            .map(|p| PairedServer {
                address: p.address,
                device: p.device,
            }))
    }

    /// Pair with the server that the code names. The pin check is before any byte is sent, and the
    /// token of the answer goes to the store.
    pub async fn pair(&self, code: &str, device_name: &str) -> Result<PairedServer, ClientError> {
        let code = code.trim();
        let decoded = PairingCode::decode(code)
            .map_err(|_| ClientError::new(ErrorKind::CodeRefused, "the code is not accepted"))?;
        let body = serde_json::to_vec(&PairRequest {
            code: code.to_string(),
            device_name: device_name.to_string(),
        })
        .map_err(|_| {
            ClientError::new(ErrorKind::BadRequest, "the pair request cannot be written")
        })?;
        let response = pinned_call(
            &decoded.listen,
            &decoded.spki_sha256,
            "POST",
            "/v1/pair",
            None,
            Some(body),
            self.timeouts,
        )
        .await?;
        if response.status != 200 {
            return Err(error_from_status(
                response.status,
                Route::Pair,
                &response.body,
                &[code],
            ));
        }
        let result: PairResult =
            serde_json::from_slice(&response.body).map_err(|_| not_documented())?;
        let paired = Paired {
            address: decoded.listen.clone(),
            pin: pf_api::code::hex(&decoded.spki_sha256),
            token: result.token,
            device: result.device,
        };
        if !paired.is_valid() {
            return Err(ClientError::new(
                ErrorKind::Server,
                "the server's pair answer is not the documented one",
            ));
        }
        let server = PairedServer {
            address: paired.address.clone(),
            device: paired.device.clone(),
        };
        self.on_store(move |store| store.save(&paired)).await?;
        Ok(server)
    }

    /// One call with the device token on the pinned connection.
    async fn token_call(&self, request: ApiRequest) -> Result<Value, ClientError> {
        let paired = self.on_store(|store| store.load()).await?.ok_or_else(|| {
            ClientError::new(ErrorKind::Unauthorized, "this device is not paired")
        })?;
        let pin = parse_pin(&paired.pin)
            .ok_or_else(|| ClientError::new(ErrorKind::Server, "the stored pin is not valid"))?;
        let response = pinned_call(
            &paired.address,
            &pin,
            request.method,
            &request.path,
            Some(&paired.token),
            request.body,
            self.timeouts,
        )
        .await?;
        if response.status != 200 {
            return Err(error_from_status(
                response.status,
                Route::Token,
                &response.body,
                &[&paired.token],
            ));
        }
        match serde_json::from_slice::<Value>(&response.body) {
            Ok(v) if !v.is_null() => Ok(v),
            _ => Err(ClientError::new(
                ErrorKind::Server,
                "the server's answer is not JSON",
            )),
        }
    }

    /// `GET /v1/status`.
    pub async fn status(&self) -> Result<ApiStatus, ClientError> {
        let request = ApiRequest {
            method: "GET",
            path: "/v1/status".to_string(),
            body: None,
        };
        parse_answer(self.token_call(request).await?)
    }

    /// `POST /v1/verbs/<verb>`. A verb off the allowlist is a `bad_request` before any connection.
    pub async fn verb(&self, verb: &str, args: Value) -> Result<Value, ClientError> {
        let request = verb_request(verb, &args)?;
        self.token_call(request).await
    }

    pub async fn project_list(&self) -> Result<ProjectList, ClientError> {
        parse_answer(self.verb("project_list", serde_json::json!({})).await?)
    }

    pub async fn project_add(&self, args: ProjectAddArgs) -> Result<ProjectChanged, ClientError> {
        let value = serde_json::to_value(&args).map_err(|_| {
            ClientError::new(ErrorKind::BadRequest, "the arguments cannot be written")
        })?;
        parse_answer(self.verb("project_add", value).await?)
    }

    /// Drop the token and the pin of this device. The server still lists the device until a revoke.
    pub async fn forget(&self) -> Result<(), ClientError> {
        self.on_store(|store| store.forget()).await
    }
}
