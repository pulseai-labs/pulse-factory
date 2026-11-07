//! The Pulse Factory Mac app: a Tauri 2 shell over the UI bundle (`apps/ui`).
//!
//! The six Tauri commands are the six operations of the UI's transport interface
//! (`docs/contracts/ui-transport.md`), with the same names. Each calls `pf-client`'s
//! `Transport<KeychainStore>`: the pinned TLS call, and the device token in the macOS Keychain. A
//! command returns the wire type, or rejects with `{"kind": …, "message": …}`, the same shape as the
//! test bridge's error. No command takes or returns the device token, and the webview opens no
//! network connection (its CSP allows the bundle's own assets and Tauri's IPC only).

use pf_api::types::{ApiStatus, ProjectAddArgs, ProjectChanged, ProjectList};
use pf_client::error::ClientError;
use pf_client::keychain::KeychainStore;
use pf_client::transport::{PairedServer, Transport};
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
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            paired,
            pair,
            status,
            project_list,
            project_add,
            forget
        ])
        .run(tauri::generate_context!())
        .expect("the Pulse Factory app failed to run");
}
