//! `pf-client-bridge --ui <dir> --state <dir>`: the test bridge (`docs/contracts/ui-transport.md`).
//!
//! It serves the built bundle and the bridge protocol on `127.0.0.1` on a free port and prints one
//! line, `listening http://127.0.0.1:<port>`. Its device store is `<state>/device.json`. It is for
//! `scripts/demo/ui-pair.sh` (d12) and tests. The Mac app does not use it.

use std::path::PathBuf;

const USAGE: &str = "usage: pf-client-bridge --ui <dir> --state <dir>";

fn parse_args() -> Option<(PathBuf, PathBuf)> {
    let mut ui = None;
    let mut state = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next()?;
        match flag.as_str() {
            "--ui" => ui = Some(PathBuf::from(value)),
            "--state" => state = Some(PathBuf::from(value)),
            _ => return None,
        }
    }
    Some((ui?, state?))
}

fn main() {
    let Some((ui, state)) = parse_args() else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cannot start the runtime: {e}");
            std::process::exit(1);
        }
    };
    runtime.block_on(async {
        match pf_client::bridge::serve(ui, state).await {
            Ok(bridge) => {
                println!("listening {}", bridge.url());
                bridge.wait().await;
            }
            Err(message) => {
                eprintln!("{message}");
                std::process::exit(2);
            }
        }
    });
}
