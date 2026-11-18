//! AC-7: the systemd launcher without a bus: the `systemd-run` argument list it builds, the unit
//! and slice names, the parsers of what `systemctl` prints, and the `stop-post` step. A real unit
//! is the work of `systemd_live.rs`.

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{KEY, Tmp, host_bin};
use pf_host::launcher::{
    HostRef, HostState, Launcher, StartSpec, Systemd, active_state, parse_list_units, parse_show,
    quote_exec_word,
};
use pf_host::names;

const SECRET_VALUE: &str = "tok-4e77-secret-value";

fn spec(tmp: &Tmp) -> StartSpec {
    let mut spec = StartSpec::new(
        "srv",
        "S-1",
        3,
        tmp.path.join("rec.json"),
        tmp.path.join("state"),
        PathBuf::from("/opt/pf/hosts/abc/pf-host"),
        KEY.to_string(),
    );
    spec.secrets = BTreeMap::from([("API_TOKEN".to_string(), SECRET_VALUE.to_string())]);
    spec.controller_cgroup = Some("/user.slice/pulse-factory.service".to_string());
    spec
}

fn args(tmp: &Tmp) -> Vec<String> {
    Systemd::new(tmp.path.join("rt"))
        .systemd_run_args(
            &spec(tmp),
            &tmp.path.join("rt/pulse-factory/secrets/u.json"),
        )
        .unwrap()
}

#[test]
fn the_argument_list_carries_the_unit_settings_of_spike_a() {
    let tmp = Tmp::new();
    let a = args(&tmp);
    let has = |s: &str| a.iter().any(|x| x == s);
    assert!(has("--user"));
    assert!(has("--collect"));
    assert!(has("--unit=pf-host-srv-S-1-3"));
    assert!(has("--slice=pf-hosts-srv.slice"));
    // Every property rides behind its own `-p`.
    let props: Vec<&String> = a
        .windows(2)
        .filter(|w| w[0] == "-p")
        .map(|w| &w[1])
        .collect();
    for want in [
        "Type=notify",
        "KillMode=mixed",
        "RuntimeDirectory=pf-host-srv-S-1-3",
        "RuntimeDirectoryMode=0700",
    ] {
        assert!(
            props.iter().any(|p| p.as_str() == want),
            "no -p {want} in {a:?}"
        );
    }
    let stop_post = props
        .iter()
        .find_map(|p| p.strip_prefix("ExecStopPost="))
        .expect("an ExecStopPost property");
    assert!(
        stop_post.starts_with(
            "/opt/pf/hosts/abc/pf-host stop-post --unit pf-host-srv-S-1-3 --state-dir "
        ),
        "{stop_post}"
    );
    assert!(
        stop_post.contains("--session S-1 --incarnation 3"),
        "{stop_post}"
    );
    // The host's own command follows `--`.
    let at = a.iter().position(|x| x == "--").unwrap();
    assert_eq!(a[at + 1], "/opt/pf/hosts/abc/pf-host");
    assert_eq!(a[at + 2], "run");
    assert!(
        a[at..]
            .windows(2)
            .any(|w| w[0] == "--launcher" && w[1] == "systemd")
    );
    assert!(
        a[at..]
            .windows(2)
            .any(|w| w[0] == "--controller-cgroup" && w[1] == "/user.slice/pulse-factory.service")
    );
    assert!(
        a[at..]
            .windows(2)
            .any(|w| w[0] == "--dir" && w[1].ends_with("/rt/pf-host-srv-S-1-3"))
    );
}

#[test]
fn the_argument_list_never_carries_setenv_a_secret_value_or_the_attach_key() {
    let tmp = Tmp::new();
    let a = args(&tmp);
    for word in &a {
        assert_ne!(word, "--setenv");
        assert_ne!(word, "-E");
        assert!(
            !word.starts_with("--setenv=") && !word.starts_with("-E"),
            "{word}"
        );
        assert!(!word.contains(SECRET_VALUE), "a secret value in {word}");
        assert!(!word.contains(KEY), "the attach key in {word}");
        assert!(!word.starts_with("Environment="), "{word}");
    }
    // The secrets reach the host by the path of a file.
    assert!(
        a.iter()
            .any(|w| w.ends_with("/rt/pulse-factory/secrets/u.json"))
    );
}

#[test]
fn unit_and_slice_names_follow_g9_and_round_trip_to_session_and_incarnation() {
    assert_eq!(
        names::unit_name("srv", "S-1", 3).unwrap(),
        "pf-host-srv-S-1-3"
    );
    assert_eq!(names::slice_name("srv").unwrap(), "pf-hosts-srv.slice");
    for (server, session, inc) in [
        ("srv", "S-1", 3u64),
        ("a1b2", "chat", 12),
        ("x.y", "s_9:z", 1),
        ("srv", "7", 0),
    ] {
        let unit = names::unit_name(server, session, inc).unwrap();
        assert_eq!(
            names::parse_unit(server, &unit),
            Some((session.to_string(), inc)),
            "{unit}"
        );
        assert_eq!(
            names::parse_unit(server, &format!("{unit}.service")),
            Some((session.to_string(), inc))
        );
    }
    // Another server's unit, or a unit that is not a host's, names nothing.
    assert_eq!(names::parse_unit("other", "pf-host-srv-S-1-3"), None);
    assert_eq!(names::parse_unit("srv", "pulse-factory.service"), None);
    assert_eq!(names::parse_unit("srv", "pf-host-srv-S-1-x"), None);
    // A name systemd would need to escape is refused, not escaped.
    for bad in ["", "a b", "a/b", "a\\b", ".hidden", &"x".repeat(65)] {
        assert!(names::unit_name("srv", bad, 1).is_err(), "{bad:?}");
        assert!(names::unit_name(bad, "S", 1).is_err(), "{bad:?}");
    }
}

#[test]
fn a_logged_spec_shows_neither_the_attach_key_nor_a_secret_value() {
    let tmp = Tmp::new();
    let shown = format!("{:?}", spec(&tmp));
    assert!(
        !shown.contains(KEY) && !shown.contains(SECRET_VALUE),
        "{shown}"
    );
    // It still names the secret variables, so a log says which ones a launch carried.
    assert!(shown.contains("API_TOKEN"), "{shown}");
}

#[test]
fn quoting_keeps_a_word_one_word() {
    assert_eq!(quote_exec_word("/a/b-c_d.e"), "/a/b-c_d.e");
    assert_eq!(quote_exec_word("a b"), "\"a b\"");
    assert_eq!(quote_exec_word("a\"b"), "\"a\\\"b\"");
    assert_eq!(quote_exec_word("a$b%c"), "\"a$$b%%c\"");
    assert_eq!(quote_exec_word(""), "\"\"");
}

#[test]
fn the_parsers_read_what_systemctl_prints() {
    let show = parse_show("ActiveState=active\nInvocationID=0123abcd\nMainPID=4242\n");
    assert_eq!(show["MainPID"], "4242");
    assert_eq!(show["InvocationID"], "0123abcd");
    for (text, state) in [
        ("active", HostState::Live),
        ("activating", HostState::Live),
        ("deactivating", HostState::Live),
        ("inactive", HostState::Gone),
        ("failed", HostState::Gone),
        ("", HostState::Unknown),
    ] {
        assert_eq!(active_state(text), state, "{text}");
    }
    let listing = "\
pf-host-srv-S-1-3.service loaded active running pf-host S-1
pf-host-srv-S-2-1.service loaded failed failed pf-host S-2
\u{25cf} pf-host-srv-S-9-4.service not-found inactive dead pf-host-srv-S-9-4.service
pf-host-other-S-1-1.service loaded active running not this server's
";
    let hosts = parse_list_units("srv", listing);
    assert_eq!(
        hosts,
        vec![
            HostRef {
                unit: "pf-host-srv-S-1-3".into(),
                session: "S-1".into(),
                incarnation: 3,
                state: HostState::Live
            },
            HostRef {
                unit: "pf-host-srv-S-2-1".into(),
                session: "S-2".into(),
                incarnation: 1,
                state: HostState::Gone
            },
            HostRef {
                unit: "pf-host-srv-S-9-4".into(),
                session: "S-9".into(),
                incarnation: 4,
                state: HostState::Gone
            },
        ]
    );
}

/// A script that stands in for `systemd-run` or `systemctl`.
fn script(dir: &Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.display().to_string()
}

#[test]
fn a_failed_systemd_run_is_an_error_naming_its_stderr_with_no_fallback_and_no_secrets_file() {
    let tmp = Tmp::new();
    let rt = tmp.path.join("rt");
    std::fs::create_dir_all(&rt).unwrap();
    let run = script(
        &tmp.path,
        "systemd-run",
        "echo 'Failed to connect to user scope bus: boom-7' >&2\nexit 1",
    );
    let ctl = script(&tmp.path, "systemctl", "exit 1");
    let launcher = Systemd::new(rt.clone()).with_programs(&run, &ctl);
    let err = launcher.start(&spec(&tmp)).expect_err("systemd-run failed");
    assert!(err.to_string().contains("boom-7"), "{err}");
    // No fallback: no detached host directory, and no secrets file left behind.
    assert!(!rt.join("pf-hosts").exists());
    let secrets = rt.join("pulse-factory/secrets");
    assert_eq!(std::fs::read_dir(&secrets).unwrap().count(), 0);
    assert_eq!(
        std::fs::metadata(&secrets).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[test]
fn start_state_stop_and_list_use_the_systemctl_program() {
    let tmp = Tmp::new();
    let rt = tmp.path.join("rt");
    std::fs::create_dir_all(&rt).unwrap();
    let log = tmp.path.join("calls.log");
    let run = script(
        &tmp.path,
        "systemd-run",
        &format!(
            "printf '%s\\n' \"$@\" > {}\nexit 0",
            tmp.path.join("run.args").display()
        ),
    );
    let ctl = script(
        &tmp.path,
        "systemctl",
        &format!(
            "echo \"$@\" >> {log}\n\
             case \"$2\" in\n\
               show) printf 'ActiveState=active\\nInvocationID=0123abcd\\nMainPID=4242\\n' ;;\n\
               list-units) echo 'pf-host-srv-S-1-3.service loaded active running pf-host' ;;\n\
               stop) exit 0 ;;\n\
             esac",
            log = log.display()
        ),
    );
    let launcher = Systemd::new(rt.clone()).with_programs(&run, &ctl);
    let started = launcher.start(&spec(&tmp)).unwrap();
    assert_eq!(started.unit, "pf-host-srv-S-1-3");
    assert_eq!(started.pid, 4242);
    assert_eq!(started.invocation_id, "0123abcd");
    assert_eq!(
        started.socket,
        rt.join("pf-host-srv-S-1-3").join("host.sock")
    );
    // The secrets file was written for the host, at 0600, and holds the key and the secret.
    let secrets = rt.join("pulse-factory/secrets/pf-host-srv-S-1-3.json");
    assert_eq!(
        std::fs::metadata(&secrets).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let body = std::fs::read_to_string(&secrets).unwrap();
    assert!(body.contains(KEY) && body.contains(SECRET_VALUE));
    // `systemd-run` got the argument list and none of it names the secrets.
    let given = std::fs::read_to_string(tmp.path.join("run.args")).unwrap();
    assert!(given.lines().any(|l| l == "--collect"));
    assert!(!given.contains(KEY) && !given.contains(SECRET_VALUE));
    assert_eq!(launcher.state("pf-host-srv-S-1-3"), HostState::Live);
    launcher.stop("pf-host-srv-S-1-3").unwrap();
    let hosts = launcher.list("srv").unwrap();
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].session, "S-1");
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        calls.contains("--user show -p ActiveState,InvocationID,MainPID pf-host-srv-S-1-3.service"),
        "{calls}"
    );
    assert!(
        calls.contains("--user stop pf-host-srv-S-1-3.service"),
        "{calls}"
    );
    assert!(calls.contains("list-units --all"), "{calls}");
    // With no user manager the state is unknown, not gone.
    let none = script(&tmp.path, "systemctl-down", "echo 'no bus' >&2\nexit 1");
    let down = Systemd::new(rt).with_programs(&run, &none);
    assert_eq!(down.state("pf-host-srv-S-1-3"), HostState::Unknown);
}

fn stop_post(tmp: &Tmp, env: &[(&str, &str)]) {
    let mut cmd = Command::new(host_bin());
    cmd.args([
        "stop-post",
        "--unit",
        "pf-host-srv-S-1-3",
        "--session",
        "S-1",
        "--incarnation",
        "3",
        "--state-dir",
    ])
    .arg(tmp.path.join("state"))
    .env_remove("SERVICE_RESULT")
    .env_remove("EXIT_CODE")
    .env_remove("EXIT_STATUS");
    for (k, v) in env {
        cmd.env(k, v);
    }
    assert!(cmd.status().unwrap().success());
}

#[test]
fn stop_post_writes_an_exit_file_when_none_exists_and_leaves_an_existing_one_alone() {
    let tmp = Tmp::new();
    let file = tmp.path.join("state/hosts/exits/pf-host-srv-S-1-3.json");
    stop_post(
        &tmp,
        &[
            ("SERVICE_RESULT", "signal"),
            ("EXIT_CODE", "killed"),
            ("EXIT_STATUS", "KILL"),
        ],
    );
    let body: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(body["written_by"], "stop-post");
    assert_eq!(body["service_result"], "signal");
    assert_eq!(body["exit_code"], "killed");
    assert_eq!(body["exit_status"], "KILL");
    assert_eq!(body["unit"], "pf-host-srv-S-1-3");
    assert_eq!(body["session"], "S-1");
    assert_eq!(body["incarnation"], 3);
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // A second run, with other values, changes nothing.
    let before = std::fs::read(&file).unwrap();
    stop_post(
        &tmp,
        &[
            ("SERVICE_RESULT", "success"),
            ("EXIT_CODE", "exited"),
            ("EXIT_STATUS", "0"),
        ],
    );
    assert_eq!(std::fs::read(&file).unwrap(), before);
    // A file the host wrote is left alone too.
    std::fs::write(&file, "{\"written_by\":\"host\"}\n").unwrap();
    stop_post(&tmp, &[("SERVICE_RESULT", "signal")]);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "{\"written_by\":\"host\"}\n"
    );
    // No stray temporary file stays behind.
    let names: Vec<_> = std::fs::read_dir(file.parent().unwrap())
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(names.len(), 1);
}

#[test]
fn a_start_that_cannot_read_the_unit_back_stops_it_and_leaves_no_secrets_file() {
    let tmp = Tmp::new();
    let rt = tmp.path.join("rt");
    std::fs::create_dir_all(&rt).unwrap();
    let log = tmp.path.join("calls.log");
    let run = script(&tmp.path, "systemd-run", "exit 0");
    // `systemd-run` succeeds, so a unit runs; `systemctl show` then fails.
    let ctl = script(
        &tmp.path,
        "systemctl",
        &format!(
            "echo \"$@\" >> {log}\n\
             case \"$2\" in\n\
               show) echo 'bus gone' >&2; exit 1 ;;\n\
             esac",
            log = log.display()
        ),
    );
    let launcher = Systemd::new(rt.clone()).with_programs(&run, &ctl);
    assert!(launcher.start(&spec(&tmp)).is_err());
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        calls.contains("--user stop pf-host-srv-S-1-3.service"),
        "an Err left a live unit: {calls}"
    );
    assert_eq!(
        std::fs::read_dir(rt.join("pulse-factory/secrets"))
            .unwrap()
            .count(),
        0
    );
}
