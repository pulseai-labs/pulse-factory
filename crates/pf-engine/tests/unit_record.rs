//! AC-7: a record round-trips through JSON; a record with an unknown `version` is refused, and
//! the error names the supported version.

use std::collections::BTreeMap;

use pf_engine::port::unit_source::{SUPPORTED_VERSION, UnitRecord, UnitRecordError};
use proptest::prelude::*;

fn sample() -> UnitRecord {
    let mut vars = BTreeMap::new();
    vars.insert("zeta".to_string(), "last".to_string());
    vars.insert("alpha".to_string(), "first".to_string());
    UnitRecord::new("unit-001", "group-a", 7, vars)
}

#[test]
fn a_record_round_trips_through_json() {
    let record = sample();
    let text = record.to_json();
    assert_eq!(UnitRecord::from_json(&text).unwrap(), record);
    // Sorted keys keep the canonical JSON stable.
    assert_eq!(
        text,
        r#"{"version":1,"id":"unit-001","group":"group-a","order":7,"vars":{"alpha":"first","zeta":"last"}}"#
    );
    // Two records with the same content give the same text, whatever the insert order.
    let mut other_vars = BTreeMap::new();
    other_vars.insert("alpha".to_string(), "first".to_string());
    other_vars.insert("zeta".to_string(), "last".to_string());
    assert_eq!(
        UnitRecord::new("unit-001", "group-a", 7, other_vars).to_json(),
        text
    );
}

#[test]
fn a_record_with_an_unknown_version_is_refused_and_the_error_names_the_supported_version() {
    let text = r#"{"version":2,"id":"u","group":"g","order":1,"vars":{}}"#;
    let err = UnitRecord::from_json(text).unwrap_err();
    assert_eq!(
        err,
        UnitRecordError::UnsupportedVersion {
            found: 2,
            supported: SUPPORTED_VERSION
        }
    );
    let message = err.to_string();
    assert!(
        message.contains(&format!("supported version is {SUPPORTED_VERSION}")),
        "{message}"
    );
    assert!(message.contains('2'), "{message}");
}

#[test]
fn the_version_is_checked_before_the_other_fields() {
    // A future version may have other fields, so it gets the version error and not a field error.
    let text = r#"{"version":9,"identifier":"u","extra":true}"#;
    assert!(matches!(
        UnitRecord::from_json(text).unwrap_err(),
        UnitRecordError::UnsupportedVersion { found: 9, .. }
    ));
    // The version itself is required, and it must be a whole number.
    assert!(matches!(
        UnitRecord::from_json(r#"{"id":"u"}"#).unwrap_err(),
        UnitRecordError::Malformed(_)
    ));
    assert!(matches!(
        UnitRecord::from_json(r#"{"version":"1"}"#).unwrap_err(),
        UnitRecordError::Malformed(_)
    ));
    assert!(matches!(
        UnitRecord::from_json("not json").unwrap_err(),
        UnitRecordError::Malformed(_)
    ));
}

#[test]
fn a_record_with_a_missing_field_an_extra_field_or_a_bad_id_is_refused() {
    for text in [
        r#"{"version":1,"id":"u","group":"g","order":1}"#,
        r#"{"version":1,"id":"u","group":"g","order":1,"vars":{},"extra":1}"#,
        r#"{"version":1,"id":"u","group":"g","order":-1,"vars":{}}"#,
        r#"{"version":1,"id":"u","group":"g","order":1,"vars":{"k":3}}"#,
    ] {
        assert!(
            matches!(
                UnitRecord::from_json(text).unwrap_err(),
                UnitRecordError::Malformed(_)
            ),
            "{text}"
        );
    }
    for id in ["", "has space", "a/b", "a#1", "a:b"] {
        let text = format!(r#"{{"version":1,"id":"{id}","group":"g","order":1,"vars":{{}}}}"#);
        assert_eq!(
            UnitRecord::from_json(&text).unwrap_err(),
            UnitRecordError::InvalidId(id.to_string()),
            "{id:?}"
        );
    }
}

#[test]
fn the_example_in_the_contract_document_parses() {
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/contracts/unit-record.md"
    ))
    .unwrap();
    let example = doc
        .lines()
        .find(|l| l.starts_with("{\"version\""))
        .expect("the document holds a JSON example");
    let record = UnitRecord::from_json(example).unwrap();
    assert_eq!(record.id, "unit-001");
    assert_eq!(
        record.to_json(),
        example,
        "the example is in canonical form"
    );
    assert!(doc.contains("unsupported unit record version 2; the supported version is 1"));
}

proptest! {
    #[test]
    fn any_valid_record_round_trips(
        id in "[A-Za-z0-9._-]{1,12}",
        group in ".{0,8}",
        order in any::<u32>(),
        vars in proptest::collection::btree_map(".{0,6}", ".{0,6}", 0..4),
    ) {
        let record = UnitRecord::new(id, group, order, vars);
        prop_assert_eq!(UnitRecord::from_json(&record.to_json()).unwrap(), record);
    }
}
