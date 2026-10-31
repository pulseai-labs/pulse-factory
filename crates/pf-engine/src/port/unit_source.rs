//! The unit source port, and the unit record that is its contract.
//!
//! A seat writes one versioned JSON record per unit. The engine reads records through this port.
//! The format is documented in `docs/contracts/unit-record.md`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The only record version this engine reads.
pub const SUPPORTED_VERSION: u32 = 1;

/// One record of work. Sorted keys keep the canonical JSON stable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnitRecord {
    pub version: u32,
    pub id: String,
    pub group: String,
    pub order: u32,
    pub vars: BTreeMap<String, String>,
}

/// Why a record is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnitRecordError {
    /// The text is not JSON, or a field is missing or has the wrong type.
    Malformed(String),
    /// The `version` is not the supported one.
    UnsupportedVersion { found: u64, supported: u32 },
    /// The `id` is empty or holds a character outside `A-Z a-z 0-9 . _ -`.
    InvalidId(String),
}

impl std::fmt::Display for UnitRecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnitRecordError::Malformed(why) => write!(f, "malformed unit record: {why}"),
            UnitRecordError::UnsupportedVersion { found, supported } => write!(
                f,
                "unsupported unit record version {found}; the supported version is {supported}"
            ),
            UnitRecordError::InvalidId(id) => write!(
                f,
                "invalid unit id {id:?}: use one or more of A-Z a-z 0-9 . _ -"
            ),
        }
    }
}

impl std::error::Error for UnitRecordError {}

impl UnitRecord {
    /// Build a record at the supported version.
    pub fn new(
        id: impl Into<String>,
        group: impl Into<String>,
        order: u32,
        vars: BTreeMap<String, String>,
    ) -> UnitRecord {
        UnitRecord {
            version: SUPPORTED_VERSION,
            id: id.into(),
            group: group.into(),
            order,
            vars,
        }
    }

    /// Read a record from JSON text. The version is checked before the other fields,
    /// so a record of a future version gets the version error.
    pub fn from_json(text: &str) -> Result<UnitRecord, UnitRecordError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| UnitRecordError::Malformed(e.to_string()))?;
        let version = value
            .get("version")
            .ok_or_else(|| UnitRecordError::Malformed("missing field `version`".to_string()))?
            .as_u64()
            .ok_or_else(|| {
                UnitRecordError::Malformed("`version` must be a whole number".to_string())
            })?;
        if version != u64::from(SUPPORTED_VERSION) {
            return Err(UnitRecordError::UnsupportedVersion {
                found: version,
                supported: SUPPORTED_VERSION,
            });
        }
        let record: UnitRecord =
            serde_json::from_value(value).map_err(|e| UnitRecordError::Malformed(e.to_string()))?;
        record.check()?;
        Ok(record)
    }

    /// Write the record as JSON text.
    pub fn to_json(&self) -> String {
        // A record holds only strings and numbers, so serialization cannot fail.
        serde_json::to_string(self).expect("a unit record always serializes")
    }

    /// Check the version and the id.
    pub fn check(&self) -> Result<(), UnitRecordError> {
        if self.version != SUPPORTED_VERSION {
            return Err(UnitRecordError::UnsupportedVersion {
                found: u64::from(self.version),
                supported: SUPPORTED_VERSION,
            });
        }
        let ok = !self.id.is_empty()
            && self
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if ok {
            Ok(())
        } else {
            Err(UnitRecordError::InvalidId(self.id.clone()))
        }
    }
}

/// Why a source cannot yield its records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitSourceError(pub String);

impl std::fmt::Display for UnitSourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unit source failed: {}", self.0)
    }
}

impl std::error::Error for UnitSourceError {}

/// Where a run's units come from.
pub trait UnitSource: Send {
    /// Return the records that are ready. A source may return a record again on a later call;
    /// the engine admits each unit ID once.
    fn poll(&mut self) -> Result<Vec<UnitRecord>, UnitSourceError>;
}
