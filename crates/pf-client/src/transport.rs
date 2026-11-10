//! The transport: the operations of the UI's interface (`docs/contracts/ui-transport.md`).
//!
//! `paired`, `pair`, `status`, `project_list`, `project_add`, the seven chat verbs, the device verbs
//! (`requests`, `show`, `confirm_code`, `answer`, `clear`, `ack`), the settings verbs, `run_list` and
//! `forget`, with the names of the interface in snake case, and the chat stream (`stream.rs`). The device token goes
//! from `POST /v1/pair` into the [`DeviceStore`] and no further: no method returns it, and no error
//! holds it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pf_api::code::PairingCode;
use pf_api::types::{
    ApiStatus, ConfirmCodeArgs, ConfirmIssued, DeviceAckArgs, DeviceAnswerArgs, DeviceClearArgs,
    DeviceRecord, PairRequest, PairResult, ProjectAddArgs, ProjectChanged, ProjectList,
    RequestActed, RequestDetail, RequestList, RequestsArgs, RunList, RunListArgs, ServerRef,
    SessionAnswerArgs, SessionChanged, SessionEndArgs, SessionInterruptArgs, SessionList,
    SessionListArgs, SessionSendArgs, SessionSetArgs, SessionStartArgs, SettingChanged,
    SettingsGetArgs, SettingsSetArgs, SettingsUnsetArgs, SettingsView, ShowArgs,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::watch;

use crate::error::{ClientError, ErrorKind, Route, error_from_status};
use crate::http::{RawResponse, Timeouts, header_text, pinned_call};
use crate::pin::parse_pin;
use crate::request::{ApiRequest, verb_request};
use crate::store::{DeviceStore, Paired};
use crate::stream::StreamTuning;

/// What `paired` and `pair` return: the server's address, this device's record and, once an answer
/// has named it, the server. Never the token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedServer {
    pub address: String,
    pub device: DeviceRecord,
    /// The server the last answer named (`pf-server-id` and `pf-server-name`): known only after an
    /// answer, so it is absent from the JSON before that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerRef>,
}

/// What `session_start` returns: the controller's own answer, read into the fields a client needs.
/// The full session record reaches the client in `session_list` and on the stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStarted {
    pub replayed: bool,
    pub session_id: String,
    pub worktree_id: String,
    pub worktree_dir: String,
    pub note: Option<String>,
}

/// What `session_end` returns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEnded {
    pub replayed: bool,
    pub session_id: String,
    pub note: Option<String>,
}

/// The controller's answer of `session_start`, as `ChatSessionStarted` writes it.
#[derive(Deserialize)]
struct StartedAnswer {
    replayed: bool,
    session: IdRow,
    worktree: WorktreeRow,
    #[serde(default)]
    note: Option<String>,
}

/// The controller's answer of `session_end`, as `ChatSessionEnded` writes it.
#[derive(Deserialize)]
struct EndedAnswer {
    replayed: bool,
    session: IdRow,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct IdRow {
    id: String,
}

#[derive(Deserialize)]
struct WorktreeRow {
    id: String,
    path: String,
}

/// How long `session_start` and `session_send` may take: either may open a chat process, and the
/// adapter's `initialize` waits up to 60 seconds.
pub const CHAT_OPEN_TIMEOUT: Duration = Duration::from_secs(90);

/// The operations over a device store.
pub struct Transport<S: DeviceStore> {
    store: Arc<S>,
    timeouts: Timeouts,
    chat_open_timeout: Duration,
    tuning: StreamTuning,
    /// The server the last answer named, shared by the clones of this transport.
    server: Arc<Mutex<Option<ServerRef>>>,
    /// Counts the calls of `forget`. An open stream watches it and ends when it changes.
    forgotten: Arc<watch::Sender<u64>>,
}

impl<S: DeviceStore> Clone for Transport<S> {
    fn clone(&self) -> Self {
        Transport {
            store: self.store.clone(),
            timeouts: self.timeouts,
            chat_open_timeout: self.chat_open_timeout,
            tuning: self.tuning.clone(),
            server: self.server.clone(),
            forgotten: self.forgotten.clone(),
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
    pub(crate) async fn on_store<T, F>(&self, work: F) -> Result<T, ClientError>
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
            chat_open_timeout: CHAT_OPEN_TIMEOUT,
            tuning: StreamTuning::default(),
            server: Arc::default(),
            forgotten: Arc::new(watch::channel(0).0),
        }
    }

    /// The same transport with other time limits, for tests.
    pub fn with_timeouts(mut self, timeouts: Timeouts) -> Transport<S> {
        self.timeouts = timeouts;
        self
    }

    /// The same transport with another request timeout for `session_start` and `session_send`,
    /// for tests.
    pub fn with_chat_open_timeout(mut self, timeout: Duration) -> Transport<S> {
        self.chat_open_timeout = timeout;
        self
    }

    /// The same transport with another reconnect schedule and silence limit, for tests.
    pub fn with_stream_tuning(mut self, tuning: StreamTuning) -> Transport<S> {
        self.tuning = tuning;
        self
    }

    pub(crate) fn timeouts(&self) -> Timeouts {
        self.timeouts
    }

    pub(crate) fn stream_tuning(&self) -> StreamTuning {
        self.tuning.clone()
    }

    /// A receiver that sees each call of `forget` after this one.
    pub(crate) fn subscribe_forget(&self) -> watch::Receiver<u64> {
        self.forgotten.subscribe()
    }

    fn server_lock(&self) -> std::sync::MutexGuard<'_, Option<ServerRef>> {
        self.server.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Remember a server an answer named, when its ID is this device's pin. A header that names
    /// another ID is ignored: the TLS pin is the check, and the header is a label.
    pub(crate) fn note_server(&self, pin: &str, server: &ServerRef) {
        if server.id.as_deref() == Some(pin) {
            *self.server_lock() = Some(server.clone());
        }
    }

    fn note_named(&self, pin: &str, id: Option<&str>, name: Option<&str>) {
        if let (Some(id), Some(name)) = (id, name) {
            self.note_server(
                pin,
                &ServerRef {
                    id: Some(id.to_string()),
                    name: name.to_string(),
                },
            );
        }
    }

    pub(crate) fn note_server_headers(&self, pin: &str, headers: &hyper::HeaderMap) {
        self.note_named(
            pin,
            header_text(headers, "pf-server-id").as_deref(),
            header_text(headers, "pf-server-name").as_deref(),
        );
    }

    fn note_answer(&self, pin: &str, response: &RawResponse) {
        self.note_named(
            pin,
            response.server_id.as_deref(),
            response.server_name.as_deref(),
        );
    }

    /// The remembered server, when it is the one this pin names.
    fn server_of(&self, pin: &str) -> Option<ServerRef> {
        self.server_lock()
            .clone()
            .filter(|s| s.id.as_deref() == Some(pin))
    }

    /// The paired server, or `None`. No network call.
    pub async fn paired(&self) -> Result<Option<PairedServer>, ClientError> {
        Ok(self
            .on_store(|store| store.load())
            .await?
            .map(|p| PairedServer {
                server: self.server_of(&p.pin),
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
        self.note_answer(&pf_api::code::hex(&decoded.spki_sha256), &response);
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
            server: self.server_of(&paired.pin),
        };
        self.on_store(move |store| store.save(&paired)).await?;
        Ok(server)
    }

    /// One call with the device token on the pinned connection.
    async fn token_call(&self, request: ApiRequest) -> Result<Value, ClientError> {
        self.token_call_within(request, self.timeouts).await
    }

    async fn token_call_within(
        &self,
        request: ApiRequest,
        timeouts: Timeouts,
    ) -> Result<Value, ClientError> {
        self.token_call_secret(request, timeouts, None).await
    }

    /// The call, with one more string an error must not hold: the confirm code of the call.
    async fn token_call_secret(
        &self,
        request: ApiRequest,
        timeouts: Timeouts,
        code: Option<&str>,
    ) -> Result<Value, ClientError> {
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
            timeouts,
        )
        .await?;
        self.note_answer(&paired.pin, &response);
        if response.status != 200 {
            let mut secrets = vec![paired.token.as_str()];
            secrets.extend(code);
            return Err(error_from_status(
                response.status,
                Route::Token,
                &response.body,
                &secrets,
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
    /// A `code` argument is a secret of the call: a server text that holds it is replaced
    /// (`r1.s4.w3`). `ack` merges through the forge, so it has the long timeout, as `session_start`
    /// and `session_send` have (W6).
    pub async fn verb(&self, verb: &str, args: Value) -> Result<Value, ClientError> {
        let request = verb_request(verb, &args)?;
        let code = args.get("code").and_then(Value::as_str);
        let timeouts = if verb == "ack" {
            Timeouts {
                request: self.chat_open_timeout,
                ..self.timeouts
            }
        } else {
            self.timeouts
        };
        self.token_call_secret(request, timeouts, code).await
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

    /// One chat verb with typed arguments. `chat_open` gives the verb the long request timeout:
    /// it may open a chat process.
    async fn chat_verb<A: Serialize>(
        &self,
        verb: &str,
        args: &A,
        chat_open: bool,
    ) -> Result<Value, ClientError> {
        let value = serde_json::to_value(args).map_err(|_| {
            ClientError::new(ErrorKind::BadRequest, "the arguments cannot be written")
        })?;
        let request = verb_request(verb, &value)?;
        let timeouts = if chat_open {
            Timeouts {
                request: self.chat_open_timeout,
                ..self.timeouts
            }
        } else {
            self.timeouts
        };
        self.token_call_within(request, timeouts).await
    }

    /// `session_list`: the server, the runtimes, the agents and the sessions.
    pub async fn session_list(&self, args: SessionListArgs) -> Result<SessionList, ClientError> {
        parse_answer(self.chat_verb("session_list", &args, false).await?)
    }

    /// `session_start`. It may open a chat process, so it has the long timeout; a timeout is
    /// `unreachable`, and a retry with the same `command_id` replays.
    pub async fn session_start(
        &self,
        args: SessionStartArgs,
    ) -> Result<SessionStarted, ClientError> {
        let answer: StartedAnswer =
            parse_answer(self.chat_verb("session_start", &args, true).await?)?;
        Ok(SessionStarted {
            replayed: answer.replayed,
            session_id: answer.session.id,
            worktree_id: answer.worktree.id,
            worktree_dir: answer.worktree.path,
            note: answer.note,
        })
    }

    /// `session_send`. It may open a chat process, so it has the long timeout.
    pub async fn session_send(&self, args: SessionSendArgs) -> Result<SessionChanged, ClientError> {
        parse_answer(self.chat_verb("session_send", &args, true).await?)
    }

    pub async fn session_answer(
        &self,
        args: SessionAnswerArgs,
    ) -> Result<SessionChanged, ClientError> {
        parse_answer(self.chat_verb("session_answer", &args, false).await?)
    }

    pub async fn session_interrupt(
        &self,
        args: SessionInterruptArgs,
    ) -> Result<SessionChanged, ClientError> {
        parse_answer(self.chat_verb("session_interrupt", &args, false).await?)
    }

    pub async fn session_set(&self, args: SessionSetArgs) -> Result<SessionChanged, ClientError> {
        parse_answer(self.chat_verb("session_set", &args, false).await?)
    }

    pub async fn session_end(&self, args: SessionEndArgs) -> Result<SessionEnded, ClientError> {
        let answer: EndedAnswer = parse_answer(self.chat_verb("session_end", &args, false).await?)?;
        Ok(SessionEnded {
            replayed: answer.replayed,
            session_id: answer.session.id,
            note: answer.note,
        })
    }

    /// One verb with typed arguments, over [`Transport::verb`].
    async fn typed_verb<A: Serialize, T: DeserializeOwned>(
        &self,
        verb: &str,
        args: &A,
    ) -> Result<T, ClientError> {
        let value = serde_json::to_value(args).map_err(|_| {
            ClientError::new(ErrorKind::BadRequest, "the arguments cannot be written")
        })?;
        parse_answer(self.verb(verb, value).await?)
    }

    /// `requests`: the open requests (or all with `all`) and the claim.
    pub async fn requests(&self, args: RequestsArgs) -> Result<RequestList, ClientError> {
        self.typed_verb("requests", &args).await
    }

    /// `show`: one request with its body and answer.
    pub async fn show(&self, args: ShowArgs) -> Result<RequestDetail, ClientError> {
        self.typed_verb("show", &args).await
    }

    /// `confirm_code`: asks for a one-use code that binds the subject. It has the short timeout:
    /// a timeout is `unreachable`, and a retry takes a new command ID.
    pub async fn confirm_code(&self, args: ConfirmCodeArgs) -> Result<ConfirmIssued, ClientError> {
        self.typed_verb("confirm_code", &args).await
    }

    /// `answer` from this device, spending a confirm code.
    pub async fn answer(&self, args: DeviceAnswerArgs) -> Result<RequestActed, ClientError> {
        self.typed_verb("answer", &args).await
    }

    /// `clear` from this device, spending a confirm code. The Needs you page offers no Clear (W10),
    /// but the allowlist has it.
    pub async fn clear(&self, args: DeviceClearArgs) -> Result<RequestActed, ClientError> {
        self.typed_verb("clear", &args).await
    }

    /// `ack` from this device, spending a confirm code. It merges through the forge, so it waits up
    /// to the long timeout.
    pub async fn ack(&self, args: DeviceAckArgs) -> Result<RequestActed, ClientError> {
        self.typed_verb("ack", &args).await
    }

    pub async fn settings_get(&self, args: SettingsGetArgs) -> Result<SettingsView, ClientError> {
        self.typed_verb("settings_get", &args).await
    }

    pub async fn settings_set(&self, args: SettingsSetArgs) -> Result<SettingChanged, ClientError> {
        self.typed_verb("settings_set", &args).await
    }

    pub async fn settings_unset(
        &self,
        args: SettingsUnsetArgs,
    ) -> Result<SettingChanged, ClientError> {
        self.typed_verb("settings_unset", &args).await
    }

    /// `run_list`: every run the controller holds (`r1.s4.w3`, W4).
    pub async fn run_list(&self, args: RunListArgs) -> Result<RunList, ClientError> {
        self.typed_verb("run_list", &args).await
    }

    /// Drop the token and the pin of this device, and the server it named. The server still lists
    /// the device until a revoke. Every stream this transport has open ends.
    pub async fn forget(&self) -> Result<(), ClientError> {
        *self.server_lock() = None;
        self.forgotten.send_modify(|count| *count += 1);
        self.on_store(|store| store.forget()).await
    }
}
