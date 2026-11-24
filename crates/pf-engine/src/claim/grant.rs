//! Who reads the clear grant (ADR-0005 and ADR-0009, amended).
//!
//! A project has at most one grant row with no revoke. [`live_grant`] is the one reader of "is the
//! grant live": `grant list`, the `grants` of `requests` and every act under the grant read through
//! it, so none of them can show or use a removed project's grant. [`open_grant`] reads the same row
//! whatever the project's state, and only the revoke uses it: the operator can always end what the
//! store still holds.

use crate::port::store::{ClearGrantRow, ProjectRow};

/// The project's grant that is live: its row with no revoke. A removed project's grant is not
/// live, so a removed project has none. A caller that holds no project row has no live grant.
pub fn live_grant<'a>(
    rows: &'a [ClearGrantRow],
    project: &ProjectRow,
) -> Option<&'a ClearGrantRow> {
    if project.removed_at.is_some() {
        return None;
    }
    open_grant(rows, &project.id)
}

/// The project's row with no revoke, whatever the project's state.
pub fn open_grant<'a>(rows: &'a [ClearGrantRow], project_id: &str) -> Option<&'a ClearGrantRow> {
    rows.iter()
        .find(|g| g.project_id == project_id && g.revoked_at.is_none())
}
