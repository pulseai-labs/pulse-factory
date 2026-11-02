//! AC-7: ADR-0011 must have settled its unverified OMP claims. The probes moved each one to
//! "Verified claims" with its result and date. Each check has a case over an inline bad sample
//! that must fire, so a check that stops checking is caught.

use std::path::PathBuf;

/// The topics the ADR left unverified: a label, and words that name it (compared in lower case).
const TOPICS: [(&str, &[&str]); 6] = [
    ("MCP servers", &["mcp"]),
    ("--plugin-dir", &["--plugin-dir"]),
    ("interrupting", &["interrupt"]),
    ("launch dialogs", &["dialog"]),
    ("handing off", &["hand off", "handing off", "handoff"]),
    ("subagents", &["subagent"]),
];

/// The text of a `###` section, up to the next heading of level 1 to 3.
fn section<'a>(adr: &'a str, heading: &str) -> Option<&'a str> {
    let start = adr.find(&format!("### {heading}"))?;
    let body = &adr[start..];
    let after = body.find('\n').map_or(body.len(), |i| i + 1);
    let end = body[after..].find("\n#").map_or(body.len(), |i| after + i);
    Some(&body[after..end])
}

fn has_date(line: &str) -> bool {
    let b: Vec<char> = line.chars().collect();
    b.windows(10).any(|w| {
        w[..4].iter().all(char::is_ascii_digit)
            && w[4] == '-'
            && w[5..7].iter().all(char::is_ascii_digit)
            && w[7] == '-'
            && w[8..].iter().all(char::is_ascii_digit)
    })
}

/// The topics that the "Unverified claims" section still names.
fn still_unverified(adr: &str) -> Vec<&'static str> {
    let Some(text) = section(adr, "Unverified claims") else {
        return Vec::new();
    };
    let lower = text.to_lowercase();
    TOPICS
        .iter()
        .filter(|(_, words)| words.iter().any(|w| lower.contains(w)))
        .map(|(label, _)| *label)
        .collect()
}

/// The topics that have no dated line in "Verified claims".
fn missing_verified(adr: &str) -> Vec<&'static str> {
    let text = section(adr, "Verified claims").unwrap_or_default();
    TOPICS
        .iter()
        .filter(|(_, words)| {
            !text.lines().any(|l| {
                let lower = l.to_lowercase();
                has_date(l) && words.iter().any(|w| lower.contains(w))
            })
        })
        .map(|(label, _)| *label)
        .collect()
}

fn adr() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/adr/adr-0011-agent-adapter-port-omp-first.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path:?}: {e}"))
}

#[test]
fn the_adr_names_no_unverified_omp_claim() {
    let left = still_unverified(&adr());
    assert!(left.is_empty(), "still unverified in ADR-0011: {left:?}");
}

#[test]
fn the_adr_has_a_dated_verified_line_for_each_claim() {
    let missing = missing_verified(&adr());
    assert!(
        missing.is_empty(),
        "no dated line in 'Verified claims' for: {missing:?}"
    );
}

#[test]
fn the_adr_keeps_its_earlier_verified_claims() {
    let adr = adr();
    let text = section(&adr, "Verified claims").expect("the section exists");
    assert!(text.contains("agent_end"), "the spike's claims stay");
}

// ---- each check fires on a bad sample

const BAD_UNVERIFIED: &str = "### Verified claims\n- x 2026-11-01\n\n### Unverified claims\n- Interrupting a session (duty 4).\n- Whether OMP loads MCP servers.\n\n### Spike contract\n";

#[test]
fn the_unverified_check_fires_on_a_sample_that_still_lists_claims() {
    let left = still_unverified(BAD_UNVERIFIED);
    assert!(
        left.contains(&"interrupting") && left.contains(&"MCP servers"),
        "{left:?}"
    );
}

#[test]
fn the_unverified_check_passes_a_section_that_names_none() {
    let ok = "### Unverified claims\nNone.\n\n### Spike contract\n- handoff in a later section\n";
    assert!(
        still_unverified(ok).is_empty(),
        "text after the section is not read"
    );
}

#[test]
fn the_verified_check_fires_on_a_missing_topic_and_on_an_undated_line() {
    let sample = "### Verified claims\n- 2026-11-01: interrupting works by Escape.\n- Launch dialogs: none seen.\n- MCP servers load (2026-11-01).\n\n### Unverified claims\n";
    let missing = missing_verified(sample);
    assert!(
        missing.contains(&"launch dialogs"),
        "an undated line does not count: {missing:?}"
    );
    assert!(
        missing.contains(&"handing off")
            && missing.contains(&"subagents")
            && missing.contains(&"--plugin-dir")
    );
    assert!(!missing.contains(&"interrupting") && !missing.contains(&"MCP servers"));
    assert_eq!(missing_verified("no sections at all").len(), 6);
}

#[test]
fn the_date_check_reads_an_iso_date_only() {
    assert!(has_date("on 2026-11-01 it held"));
    assert!(!has_date("on 26-11-01 it held"));
    assert!(!has_date("no date"));
}
