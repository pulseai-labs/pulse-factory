//! AC-2 (r1.s1.w2, property test): over random `<scheme>://<userinfo>@<host><port><path>` text,
//! `redact` replaces the user info by the one rule and keeps the host, port and path. A second pass
//! changes nothing, and `one_line_reason` holds the same redacted user info.
//!
//! The shared cargo config turns proptest's failure persistence off. The config below does too, so
//! a run from another directory leaves no `*.proptest-regressions` file.

use pf_engine::status::{REDACTED, one_line_reason, redact};
use proptest::prelude::*;

#[derive(Clone, Debug)]
struct Case {
    text: String,
    /// What follows the user info: `@<host><port><path>`.
    tail: String,
    user: String,
    password: Option<String>,
}

fn scheme() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("http".to_string()),
        Just("https".to_string()),
        Just("ssh".to_string()),
        Just("git".to_string()),
        Just("file".to_string()),
        "[a-z]{2,8}",
    ]
}

/// One to 40 printable ASCII characters, none of white space, `/`, `?`, `#`, `@` or `:`.
fn part(min: usize, max: usize) -> impl Strategy<Value = String> {
    proptest::collection::vec(
        proptest::char::range('!', '~').prop_filter("not a delimiter", |c| {
            !matches!(c, '/' | '?' | '#' | '@' | ':')
        }),
        min..=max,
    )
    .prop_map(|v| v.into_iter().collect())
}

/// The user info: a lone part, or two parts joined by one `:`. Together they are 1 to 40 characters.
fn user_info() -> impl Strategy<Value = (String, Option<String>)> {
    prop_oneof![
        part(1, 40).prop_map(|u| (u, None)),
        (part(0, 19), part(0, 20)).prop_map(|(u, p)| (u, Some(p))),
    ]
    .prop_filter("one to 40 characters", |(u, p)| {
        let n = u.chars().count() + p.as_ref().map_or(0, |p| p.chars().count() + 1);
        (1..=40).contains(&n)
    })
}

fn case() -> impl Strategy<Value = Case> {
    (
        scheme(),
        user_info(),
        "[a-z0-9.-]{1,20}",
        proptest::option::of(1u16..=65535),
        "/[a-z0-9/._-]{0,30}",
    )
        .prop_map(|(scheme, (user, password), host, port, path)| {
            let info = match &password {
                Some(p) => format!("{user}:{p}"),
                None => user.clone(),
            };
            let port = port.map(|p| format!(":{p}")).unwrap_or_default();
            let tail = format!("@{host}{port}{path}");
            Case {
                text: format!("{scheme}://{info}{tail}"),
                tail,
                user,
                password,
            }
        })
}

/// The user info a redacted text carries: what lies between `://` and the last `@`.
fn redacted_info(out: &str) -> &str {
    let after = &out[out.find("://").unwrap() + 3..];
    &after[..after.rfind('@').unwrap()]
}

/// The user info the rule leaves for this case.
fn expected_info(c: &Case) -> String {
    match &c.password {
        // `user:secret`: the name stays, the secret goes.
        Some(p) if !p.is_empty() => format!("{}:{REDACTED}", c.user),
        // `token:`: the part before the colon is the credential.
        Some(_) => format!("{REDACTED}:"),
        // `token`: the whole user info goes.
        None => REDACTED.to_string(),
    }
}

/// Does the text hold the start of a secret that another rule of `redact` cuts?
///
/// This test is about the user info of a URL. The host, the path and the user info are random, so
/// one of them can spell a token such as `sk-` and 16 more characters, and `redact` cuts it by its
/// own rule. The tail then changes by design. A text with such a start belongs to the other rules'
/// tests, so the strategy leaves it out. The check is a plain, case-blind search, so it leaves out
/// a little more than `redact` cuts. That costs nothing.
fn holds_another_rule(text: &str) -> bool {
    const STARTS: [&str; 13] = [
        "sk-",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "bearer",
        "authorization",
        "--api-key",
        "_token=",
        "_key=",
        "_secret=",
    ];
    let lower = text.to_ascii_lowercase();
    STARTS.iter().any(|s| lower.contains(s))
}

/// The three texts that failed `cargo test --workspace` (r1.s2.w4 correction, 1 run in 1000). The
/// path or the host held a token that another rule cuts, so the tail changed by design.
const SET_ASIDE: [&str; 3] = [
    "http://0@a/sk-__-a000_____aa_a",
    "http://A@a/gho_a0a__aa_0_000aa_",
    "http://a@0/sk-0_aa0a-aa0aa__00",
];

#[test]
fn a_text_with_another_rules_token_is_set_aside_and_a_plain_url_is_not() {
    for text in SET_ASIDE {
        let out = redact(text);
        let tail = &text[text.find('@').unwrap()..];
        assert!(
            !out.ends_with(tail),
            "{text} -> {out}: the other rule did not cut"
        );
        assert!(holds_another_rule(text), "{text} was not set aside");
    }
    for text in [
        "https://user:pw@host:8080/a/b.c",
        "ssh://token@10.0.0.1/repo.git",
        "git://u:@h.example/skip-this/ghost_dir",
    ] {
        assert!(!holds_another_rule(text), "{text} was set aside");
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn the_user_info_of_every_url_follows_the_one_rule(
        c in case().prop_filter("no other rule's token", |c| !holds_another_rule(&c.text))
    ) {
        let once = redact(&c.text);
        prop_assert_eq!(&redact(&once), &once, "not idempotent");
        prop_assert!(once.ends_with(&c.tail), "tail changed: {} -> {}", c.text, once);
        prop_assert_eq!(redacted_info(&once), expected_info(&c), "{} -> {}", c.text, once);
        let reason = one_line_reason(&c.text);
        prop_assert_eq!(redacted_info(&reason), expected_info(&c), "{} -> {}", c.text, reason);
    }
}
