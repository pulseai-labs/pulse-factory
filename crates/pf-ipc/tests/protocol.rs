//! AC-1: the CLI-to-controller protocol (ADR-0001, `docs/contracts/cli.md`).

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use pf_ipc::{
    ClientError, Handler, MAX_MESSAGE_BYTES, PROTOCOL_VERSION, Request, Response, bind, call,
    socket_path, state_dir_from,
};
use serde_json::{Value, json};

/// Answers `echo` with its arguments and everything else with an error.
struct Echo;

impl Handler for Echo {
    fn handle(&self, request: Request) -> Response {
        match request.verb.as_str() {
            "echo" => Response::ok(request.args),
            other => Response::err(format!("unknown verb {other}")),
        }
    }
}

struct Running {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Bind the socket of `dir` and serve `Echo` on a thread of its own.
fn start(dir: &Path) -> Running {
    let bound = bind(dir).expect("bind the socket");
    let (stop, rx) = tokio::sync::oneshot::channel::<()>();
    let thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(pf_ipc::serve(bound, Arc::new(Echo), async {
            let _ = rx.await;
        }));
    });
    Running {
        stop: Some(stop),
        thread: Some(thread),
    }
}

fn raw(dir: &Path, bytes: &[u8]) -> String {
    let mut s = UnixStream::connect(socket_path(dir)).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    // The server may close before it has read everything. A write error is fine.
    let _ = s.write_all(bytes);
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

#[test]
fn a_round_trip_works() {
    let tmp = tempfile::tempdir().unwrap();
    let _server = start(tmp.path());
    let answer = call(tmp.path(), "echo", json!({"a": 1, "b": ["x"]})).expect("call");
    assert!(answer.ok);
    assert_eq!(answer.protocol_version, PROTOCOL_VERSION);
    assert_eq!(answer.result, json!({"a": 1, "b": ["x"]}));
    let refused = call(tmp.path(), "nope", Value::Null).expect("call");
    assert!(!refused.ok);
    assert!(refused.error.unwrap().contains("unknown verb nope"));
}

#[test]
fn another_version_is_refused_and_names_both() {
    let tmp = tempfile::tempdir().unwrap();
    let _server = start(tmp.path());
    let text = raw(
        tmp.path(),
        b"{\"protocol_version\":2,\"verb\":\"echo\",\"args\":{}}\n",
    );
    let answer: Value = serde_json::from_str(text.trim()).expect("an answer line");
    assert_eq!(answer["ok"], json!(false));
    let why = answer["error"].as_str().unwrap();
    assert!(
        why.contains('2') && why.contains(&PROTOCOL_VERSION.to_string()),
        "{why}"
    );
    // The server still answers.
    assert!(call(tmp.path(), "echo", json!(1)).unwrap().ok);
}

#[test]
fn a_client_refuses_an_answer_of_another_version_and_names_both() {
    let tmp = tempfile::tempdir().unwrap();
    // A server that speaks version 9.
    let bound = bind(tmp.path()).unwrap();
    let listener = bound.into_std_listener();
    let t = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut buf = [0u8; 256];
        let _ = s.read(&mut buf);
        s.write_all(b"{\"protocol_version\":9,\"ok\":true,\"result\":null}\n")
            .unwrap();
    });
    let err = call(tmp.path(), "echo", Value::Null).unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains('9') && text.contains(&PROTOCOL_VERSION.to_string()),
        "{text}"
    );
    t.join().unwrap();
}

#[test]
fn a_message_over_one_mebibyte_is_refused_and_the_server_lives() {
    let tmp = tempfile::tempdir().unwrap();
    let _server = start(tmp.path());
    let mut big = b"{\"protocol_version\":1,\"verb\":\"echo\",\"args\":\"".to_vec();
    big.extend(std::iter::repeat_n(b'a', MAX_MESSAGE_BYTES + 10));
    big.extend_from_slice(b"\"}\n");
    let text = raw(tmp.path(), &big);
    let answer: Value = serde_json::from_str(text.trim()).expect("an answer line");
    assert_eq!(answer["ok"], json!(false));
    assert!(answer["error"].as_str().unwrap().contains("1 MiB"));
    assert!(call(tmp.path(), "echo", json!("still here")).unwrap().ok);
}

#[test]
fn a_line_that_is_not_json_is_refused_and_the_connection_closes() {
    let tmp = tempfile::tempdir().unwrap();
    let _server = start(tmp.path());
    let text = raw(tmp.path(), b"this is not json\n");
    let answer: Value = serde_json::from_str(text.trim()).expect("an answer line");
    assert_eq!(answer["ok"], json!(false));
    assert!(answer["error"].as_str().unwrap().contains("not JSON"));
    // `read_to_string` returned, so the server closed its side.
    assert!(call(tmp.path(), "echo", json!(true)).unwrap().ok);
    // JSON that is not an object with a version is refused too.
    let text = raw(tmp.path(), b"[1,2]\n");
    assert!(text.contains("\"ok\":false"), "{text}");
}

#[test]
fn the_socket_is_0600_and_its_directory_0700() {
    let tmp = tempfile::tempdir().unwrap();
    // A state directory that does not exist yet is made with mode 0700.
    let dir = tmp.path().join("state");
    let _server = start(&dir);
    let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    let sock_mode = std::fs::metadata(socket_path(&dir))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700);
    assert_eq!(sock_mode, 0o600);
}

#[test]
fn a_client_that_cannot_connect_names_the_service() {
    let tmp = tempfile::tempdir().unwrap();
    let err = call(tmp.path(), "echo", Value::Null).unwrap_err();
    assert!(matches!(err, ClientError::NoController { .. }));
    assert!(
        err.to_string()
            .contains("systemctl --user status pulse-factory"),
        "{err}"
    );
}

#[test]
fn a_second_controller_refuses_and_a_stale_socket_does_not_stop_one() {
    let tmp = tempfile::tempdir().unwrap();
    {
        let _first = bind(tmp.path()).unwrap();
        let Err(second) = bind(tmp.path()) else {
            panic!("a second bind refuses");
        };
        assert!(second.to_string().contains("already"), "{second}");
    }
    // The first is gone, but its socket file stays: it is stale.
    assert!(socket_path(tmp.path()).exists());
    let _again = bind(tmp.path()).expect("a stale socket is removed");
}

#[test]
fn the_state_directory_follows_pf_state_dir_then_home() {
    use std::ffi::OsString;
    let set = Some(OsString::from("/tmp/pf-x"));
    let home = Some(OsString::from("/home/op"));
    assert_eq!(state_dir_from(set, home.clone()), Path::new("/tmp/pf-x"));
    assert_eq!(
        state_dir_from(Some(OsString::new()), home.clone()),
        Path::new("/home/op/.local/state/pulse-factory")
    );
    assert_eq!(
        state_dir_from(None, home),
        Path::new("/home/op/.local/state/pulse-factory")
    );
}
