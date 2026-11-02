//! Role rule files (`seat-permission-rules`, least privilege), documented in
//! `docs/contracts/role-rules.md`.
//!
//! One file per role: `{version: 1, role, allow: [...]}`. A role gets exactly what it lists.
//! `r0.s1.w3`'s `pf-rules` reads these files and adds the deny list.

use std::collections::BTreeSet;

use crate::marked::{self, Entry, Node, ParseError, Value};

/// The only rule-file version this parser reads.
pub const SUPPORTED_VERSION: u32 = 1;

/// One permission a role may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Allow {
    DeleteOwnScratch,
    DeleteOwnBuildTarget,
    PushOwnBranch,
    PushOwnTag,
    WriteOwnReportAndRunFiles,
}

impl Allow {
    pub const ALL: [Allow; 5] = [
        Allow::DeleteOwnScratch,
        Allow::DeleteOwnBuildTarget,
        Allow::PushOwnBranch,
        Allow::PushOwnTag,
        Allow::WriteOwnReportAndRunFiles,
    ];

    /// The name as written in a rule file.
    pub fn as_str(self) -> &'static str {
        match self {
            Allow::DeleteOwnScratch => "delete_own_scratch",
            Allow::DeleteOwnBuildTarget => "delete_own_build_target",
            Allow::PushOwnBranch => "push_own_branch",
            Allow::PushOwnTag => "push_own_tag",
            Allow::WriteOwnReportAndRunFiles => "write_own_report_and_run_files",
        }
    }

    pub fn parse(text: &str) -> Option<Allow> {
        Allow::ALL.into_iter().find(|a| a.as_str() == text)
    }
}

/// A parsed rule file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleRules {
    pub version: u32,
    pub role: String,
    pub allow: Vec<Allow>,
}

fn field<'a>(entries: &'a [Entry], key: &str) -> Option<&'a Entry> {
    entries.iter().find(|e| e.key == key)
}

impl RoleRules {
    /// Read a rule file. `file_name` is the file's name, such as `implementer.yaml`: the `role`
    /// field must equal the part before the extension.
    pub fn parse(text: &str, file_name: &str) -> Result<RoleRules, ParseError> {
        let root = marked::parse(text)?;
        let Value::Map(entries) = &root.value else {
            return Err(ParseError::new(root.pos, "a rule file must be a mapping"));
        };
        // The version is read first, as in the workflow file.
        let version_entry = field(entries, "version").ok_or_else(|| {
            ParseError::new(
                root.pos,
                "missing required field `version` in the rule file",
            )
        })?;
        let ok = matches!(
            &version_entry.value.value,
            Value::Scalar { text, plain: true } if text.parse::<u32>() == Ok(SUPPORTED_VERSION)
        );
        if !ok {
            let found = match &version_entry.value.value {
                Value::Scalar { text, .. } => text.clone(),
                _ => "(not text)".to_string(),
            };
            return Err(ParseError::new(
                version_entry.value.pos,
                format!(
                    "unsupported version {found}; the supported version is {SUPPORTED_VERSION}"
                ),
            ));
        }
        for e in entries {
            if !matches!(e.key.as_str(), "version" | "role" | "allow") {
                return Err(ParseError::new(
                    e.key_pos,
                    format!(
                        "unknown field `{}` in the rule file; the known fields are: version, role, allow",
                        e.key
                    ),
                ));
            }
        }
        let missing = |key: &str| {
            ParseError::new(
                root.pos,
                format!("missing required field `{key}` in the rule file"),
            )
        };
        let role_node = &field(entries, "role").ok_or_else(|| missing("role"))?.value;
        let role = scalar_text(role_node, "`role`")?;
        let stem = file_name
            .rsplit_once('.')
            .map_or(file_name, |(stem, _)| stem);
        if role != stem {
            return Err(ParseError::new(
                role_node.pos,
                format!(
                    "the rule file is named `{file_name}`, so its role must be `{stem}`, not `{role}`"
                ),
            ));
        }
        let allow_node = &field(entries, "allow")
            .ok_or_else(|| missing("allow"))?
            .value;
        let Value::Seq(list) = &allow_node.value else {
            return Err(ParseError::new(
                allow_node.pos,
                format!("`allow` must be a list, not {}", allow_node.kind()),
            ));
        };
        let mut allow = Vec::new();
        let mut seen = BTreeSet::new();
        for item in list {
            let name = scalar_text(item, "an `allow` entry")?;
            let value = Allow::parse(&name).ok_or_else(|| {
                let known: Vec<&str> = Allow::ALL.iter().map(|a| a.as_str()).collect();
                ParseError::new(
                    item.pos,
                    format!(
                        "unknown allow value `{name}`; the known values are: {}",
                        known.join(", ")
                    ),
                )
            })?;
            if !seen.insert(value) {
                return Err(ParseError::new(
                    item.pos,
                    format!("allow value `{name}` is listed twice"),
                ));
            }
            allow.push(value);
        }
        Ok(RoleRules {
            version: SUPPORTED_VERSION,
            role,
            allow,
        })
    }

    pub fn allows(&self, permission: Allow) -> bool {
        self.allow.contains(&permission)
    }
}

fn scalar_text(node: &Node, what: &str) -> Result<String, ParseError> {
    match &node.value {
        Value::Scalar { text, .. } if !text.trim().is_empty() => Ok(text.clone()),
        _ => Err(ParseError::new(
            node.pos,
            format!("{what} must be text, not {}", node.kind()),
        )),
    }
}
