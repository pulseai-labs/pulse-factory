//! AC-1 of `r1.s2.w4`: each API status maps to its kind as in `docs/contracts/ui-transport.md`, and a
//! message is cut to 300 characters and never holds a token.

use pf_client::error::{ClientError, ErrorKind, Route, error_from_status};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn body(text: &str) -> Vec<u8> {
    serde_json::json!({ "error": text })
        .to_string()
        .into_bytes()
}

#[test]
fn each_status_maps_to_its_kind() {
    let cases: [(u16, Route, ErrorKind); 11] = [
        (401, Route::Token, ErrorKind::Unauthorized),
        (401, Route::Pair, ErrorKind::CodeRefused),
        (403, Route::Token, ErrorKind::Forbidden),
        (422, Route::Token, ErrorKind::Refused),
        (422, Route::Pair, ErrorKind::Refused),
        (429, Route::Token, ErrorKind::RateLimited),
        (400, Route::Token, ErrorKind::BadRequest),
        (413, Route::Token, ErrorKind::BadRequest),
        (500, Route::Token, ErrorKind::Server),
        (502, Route::Pair, ErrorKind::Server),
        (404, Route::Token, ErrorKind::Server),
    ];
    for (status, route, kind) in cases {
        let e = error_from_status(status, route, &body("no"), &[]);
        assert_eq!(e.kind, kind, "status {status} on {route:?}");
        assert_eq!(e.message, "no");
    }
}

#[test]
fn the_nine_kinds_serialize_to_the_names_of_the_contract() {
    let kinds = [
        (ErrorKind::Unreachable, "unreachable"),
        (ErrorKind::Unauthorized, "unauthorized"),
        (ErrorKind::CodeRefused, "code_refused"),
        (ErrorKind::KeyChanged, "key_changed"),
        (ErrorKind::Forbidden, "forbidden"),
        (ErrorKind::Refused, "refused"),
        (ErrorKind::RateLimited, "rate_limited"),
        (ErrorKind::BadRequest, "bad_request"),
        (ErrorKind::Server, "server"),
    ];
    for (kind, name) in kinds {
        let e = ClientError::new(kind, "m");
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v, serde_json::json!({ "kind": name, "message": "m" }));
    }
}

#[test]
fn a_message_is_cut_to_300_characters() {
    let long = "x".repeat(900);
    let e = error_from_status(422, Route::Token, &body(&long), &[]);
    assert_eq!(e.message.chars().count(), 300);
    // Characters, not bytes: a multi-byte text is not cut inside a character.
    let wide = "é".repeat(400);
    let e = error_from_status(422, Route::Token, &body(&wide), &[]);
    assert_eq!(e.message.chars().count(), 300);
}

#[test]
fn an_answer_with_no_text_says_the_status() {
    for raw in [
        &b""[..],
        b"not json",
        b"{}",
        b"{\"error\":\"  \"}",
        b"{\"error\":5}",
    ] {
        let e = error_from_status(503, Route::Token, raw, &[]);
        assert_eq!(e.message, "the server answered 503");
    }
}

#[test]
fn a_message_never_holds_a_token_or_a_secret() {
    // A 64-hex run is a token whether or not this client knows it.
    let e = error_from_status(422, Route::Token, &body(&format!("bad {TOKEN} here")), &[]);
    assert_eq!(e.message, "the server answered 422");
    // A secret this client holds, in any shape.
    let e = error_from_status(
        422,
        Route::Pair,
        &body("the code pf1-abcdef was refused"),
        &["pf1-abcdef"],
    );
    assert_eq!(e.message, "the server answered 422");
    // A 63-digit run is not a token.
    let ok = "a".repeat(63);
    let e = error_from_status(422, Route::Token, &body(&ok), &[]);
    assert_eq!(e.message, ok);
}

#[test]
fn control_characters_become_spaces() {
    let e = error_from_status(422, Route::Token, &body("a\nb\u{7}c"), &[]);
    assert_eq!(e.message, "a b c");
}
