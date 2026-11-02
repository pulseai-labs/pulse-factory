//! The OMP version the adapter qualifies (ADR-0011).

/// The versions the spike qualified.
pub const QUALIFIED_VERSIONS: [&str; 1] = ["18.11.3"];

/// Read a version such as `18.11.3` from `omp --version` output (`omp/18.11.3` or `omp v18.11.3`).
pub fn parse_version(text: &str) -> Option<String> {
    for word in text.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        let parts: Vec<&str> = word.split('.').collect();
        if parts.len() >= 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        {
            return Some(word.to_string());
        }
    }
    None
}

/// Compare two dotted numeric versions. A missing part counts as 0.
pub fn version_at_least(version: &str, minimum: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.split('.')
            .map(|p| {
                p.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0)
            })
            .collect()
    };
    let (a, b) = (parse(version), parse(minimum));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x > y;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_both_forms() {
        assert_eq!(parse_version("omp/18.11.3\n").as_deref(), Some("18.11.3"));
        assert_eq!(parse_version("omp v18.11.3").as_deref(), Some("18.11.3"));
        assert_eq!(parse_version("no version"), None);
    }

    #[test]
    fn compares_numbers_not_text() {
        assert!(version_at_least("1.32.2", "1.26.0"));
        assert!(version_at_least("1.100.0", "1.26.0"));
        assert!(!version_at_least("1.9.0", "1.26.0"));
    }
}
