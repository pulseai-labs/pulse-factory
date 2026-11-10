//! The Pulse Factory Mac app: a Tauri 2 shell over the UI bundle (`apps/ui`).
//!
//! The Tauri commands are the operations of the UI's transport interface
//! (`docs/contracts/ui-transport.md`), with the same names: the six of the pairing and the projects,
//! the seven chat verbs, and `open_stream` and `close_stream` for the chat stream, and one `verb` command
//! for every verb added since `r1.s4` (the device verbs, the settings verbs, `run_list`, and a later
//! spine's). Each calls
//! `pf-client`'s `Transport<KeychainStore>`: the pinned TLS call, and the device token in the macOS
//! Keychain. A command returns the wire type, or rejects with `{"kind": …, "message": …}`, the same
//! shape as the test bridge's error. `verb` calls `pf-client`'s `Transport::verb`, which refuses a verb
//! that is not on `pf_api::ALLOWLIST` before it connects; a later item's verbs need no edit here. No
//! command takes or returns the device token, and the webview
//! opens no network connection (its CSP allows the bundle's own assets and Tauri's IPC only).
//!
//! The stream's events go to the webview on a `tauri::ipc::Channel`, not on Tauri's event system:
//! `listen` needs a capability that grants `core:event`, and the app declares no capability file.

use pf_api::types::{
    ApiStatus, ProjectAddArgs, ProjectChanged, ProjectList, SessionAnswerArgs, SessionChanged,
    SessionEndArgs, SessionInterruptArgs, SessionList, SessionListArgs, SessionSendArgs,
    SessionSetArgs, SessionStartArgs,
};
use pf_client::error::ClientError;
use pf_client::keychain::KeychainStore;
use pf_client::request::Value;
use pf_client::stream::{StreamEvent, StreamOptions, Streams};
use pf_client::transport::{PairedServer, SessionEnded, SessionStarted, Transport};
use tauri::{Manager, State};

type Device = Transport<KeychainStore>;

#[tauri::command]
async fn paired(device: State<'_, Device>) -> Result<Option<PairedServer>, ClientError> {
    device.paired().await
}

#[tauri::command]
async fn pair(
    device: State<'_, Device>,
    code: String,
    device_name: String,
) -> Result<PairedServer, ClientError> {
    device.pair(&code, &device_name).await
}

#[tauri::command]
async fn status(device: State<'_, Device>) -> Result<ApiStatus, ClientError> {
    device.status().await
}

#[tauri::command]
async fn project_list(device: State<'_, Device>) -> Result<ProjectList, ClientError> {
    device.project_list().await
}

#[tauri::command]
async fn project_add(
    device: State<'_, Device>,
    args: ProjectAddArgs,
) -> Result<ProjectChanged, ClientError> {
    device.project_add(args).await
}

#[tauri::command]
async fn session_list(
    device: State<'_, Device>,
    args: SessionListArgs,
) -> Result<SessionList, ClientError> {
    device.session_list(args).await
}

#[tauri::command]
async fn session_start(
    device: State<'_, Device>,
    args: SessionStartArgs,
) -> Result<SessionStarted, ClientError> {
    device.session_start(args).await
}

#[tauri::command]
async fn session_send(
    device: State<'_, Device>,
    args: SessionSendArgs,
) -> Result<SessionChanged, ClientError> {
    device.session_send(args).await
}

#[tauri::command]
async fn session_answer(
    device: State<'_, Device>,
    args: SessionAnswerArgs,
) -> Result<SessionChanged, ClientError> {
    device.session_answer(args).await
}

#[tauri::command]
async fn session_interrupt(
    device: State<'_, Device>,
    args: SessionInterruptArgs,
) -> Result<SessionChanged, ClientError> {
    device.session_interrupt(args).await
}

#[tauri::command]
async fn session_set(
    device: State<'_, Device>,
    args: SessionSetArgs,
) -> Result<SessionChanged, ClientError> {
    device.session_set(args).await
}

#[tauri::command]
async fn session_end(
    device: State<'_, Device>,
    args: SessionEndArgs,
) -> Result<SessionEnded, ClientError> {
    device.session_end(args).await
}

/// Any verb of the allowlist that has no command of its own: the device verbs, the settings verbs and
/// `run_list` (`r1.s4.w3`, W5). `pf-client` checks the name against the allowlist before it connects, so
/// a webview cannot reach a verb a device may not call. The answer is the verb's JSON.
#[tauri::command]
async fn verb(device: State<'_, Device>, name: String, args: Value) -> Result<Value, ClientError> {
    device.verb(&name, args).await
}

/// Open the chat stream. The events go to the webview on `on_event`, in order; a `closed` event is
/// the last. Returns the stream's ID for `close_stream`.
#[tauri::command]
async fn open_stream(
    device: State<'_, Device>,
    streams: State<'_, Streams>,
    options: StreamOptions,
    on_event: tauri::ipc::Channel<StreamEvent>,
) -> Result<u64, ClientError> {
    Ok(streams.open(&device, options, move |event| on_event.send(event).is_ok()))
}

/// End one stream. `false` when the ID is not open.
#[tauri::command]
async fn close_stream(streams: State<'_, Streams>, id: u64) -> Result<bool, ClientError> {
    Ok(streams.close(id))
}

#[tauri::command]
async fn forget(device: State<'_, Device>) -> Result<(), ClientError> {
    device.forget().await
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            // The address, the pin and the device record go in the app's data directory. They hold
            // no secret: the token is in the Keychain.
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            app.manage(Transport::new(std::sync::Arc::new(KeychainStore::new(dir))));
            app.manage(Streams::new());
            Ok(())
        })
        // A page that starts to load is a reload: the old page's channels are gone, so no stream
        // stays open behind a channel that nobody reads.
        .on_page_load(|webview, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Started {
                webview.state::<Streams>().close_all();
            }
        })
        .invoke_handler(tauri::generate_handler![
            paired,
            pair,
            status,
            project_list,
            project_add,
            session_list,
            session_start,
            session_send,
            session_answer,
            session_interrupt,
            session_set,
            session_end,
            verb,
            open_stream,
            close_stream,
            forget
        ])
        .run(tauri::generate_context!())
        .expect("the Pulse Factory app failed to run");
}
