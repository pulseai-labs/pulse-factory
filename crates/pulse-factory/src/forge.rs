//! The forge facts `arm` reads: the repository on GitHub, and its default branch.

use pf_workflow::commands::run_argv;

/// The `owner/name` of a GitHub remote URL. It reads `https://`, `ssh://` and `git@` forms on
/// `github.com` and `ssh.github.com`, with or without user info, a port, `.git` or a trailing `/`.
pub fn parse_origin(url: &str) -> Option<String> {
    let url = url.trim();
    let after_scheme = |scheme: &str| url.strip_prefix(scheme);
    let (host_part, path) = if let Some(rest) = after_scheme("https://") {
        rest.split_once('/')?
    } else if let Some(rest) = after_scheme("ssh://") {
        rest.split_once('/')?
    } else if !url.contains("://") {
        // `git@github.com:owner/name.git`
        url.split_once(':')?
    } else {
        return None;
    };
    let host = host_part.rsplit_once('@').map_or(host_part, |(_, h)| h);
    // The port belongs to a URL form. A scp-like form has none.
    let host = if url.contains("://") {
        host.split_once(':').map_or(host, |(h, port)| {
            if port.chars().all(|c| c.is_ascii_digit()) {
                h
            } else {
                host
            }
        })
    } else {
        host
    };
    if !matches!(
        host.to_ascii_lowercase().as_str(),
        "github.com" | "ssh.github.com"
    ) {
        return None;
    }
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    let word = |w: &str| {
        !w.is_empty()
            && !w.starts_with('-')
            && !w.starts_with('.')
            && w.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    (word(owner) && word(name)).then(|| format!("{owner}/{name}"))
}

/// What GitHub says about a repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    pub forge_repo: String,
    pub default_branch: String,
}

/// Ask `gh` for the repository's own name and default branch, so a renamed repo or a name in
/// another case still matches the PR URL.
pub fn gh_facts(owner_name: &str) -> Result<Facts, String> {
    let argv: Vec<String> = [
        "gh",
        "repo",
        "view",
        owner_name,
        "--json",
        "nameWithOwner,defaultBranchRef",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let out = run_argv(&argv, None, 30).map_err(|f| format!("gh repo view {owner_name}: {f}"))?;
    if out.exit_code != Some(0) {
        let line = String::from_utf8_lossy(&out.stderr)
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .chars()
            .take(160)
            .collect::<String>();
        return Err(format!(
            "gh repo view {owner_name} failed (exit {}): {line}",
            out.exit_code
                .map_or("signal".to_string(), |c| c.to_string())
        ));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("gh repo view {owner_name} printed no JSON: {e}"))?;
    let name = v["nameWithOwner"].as_str().unwrap_or_default();
    let branch = v["defaultBranchRef"]["name"].as_str().unwrap_or_default();
    if name.is_empty() || branch.is_empty() {
        return Err(format!(
            "gh repo view {owner_name} gave no nameWithOwner or default branch"
        ));
    }
    Ok(Facts {
        forge_repo: name.to_string(),
        default_branch: branch.to_string(),
    })
}
