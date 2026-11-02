//! Plugin and config limits (ADR-0011, "fail closed").
//!
//! The probes of 2026-11-01 (OMP 18.11.3) showed that OMP starts MCP servers from a project
//! config in the seat's directory and from a declared plugin, and that it activates their tools
//! whatever `--tools` says. `--skills` limits skills, and a plugin's hooks did not run. The
//! adapter limits what it can in the extension, and it refuses a launch for what it cannot:
//! a declared plugin that brings an MCP server, and an MCP config file that OMP would read.

use std::path::{Path, PathBuf};

/// MCP config files that OMP reads inside a directory tree.
const PROJECT_MCP_FILES: [&str; 2] = [".mcp.json", ".omp/mcp.json"];

/// MCP config files that OMP reads in the user's home.
const USER_MCP_FILES: [&str; 2] = [".omp/agent/mcp.json", ".omp/mcp.json"];

fn plugin_manifest_has_mcp(dir: &Path) -> bool {
    [
        ".claude-plugin/plugin.json",
        ".omp-plugin/plugin.json",
        "plugin.json",
    ]
    .iter()
    .filter_map(|rel| std::fs::read_to_string(dir.join(rel)).ok())
    .any(|text| text.contains("mcpServers"))
}

/// The first reason the launch must be refused, or `Ok`.
pub fn check(cwd: &Path, plugin_dirs: &[PathBuf], home: Option<&Path>) -> Result<(), String> {
    for dir in plugin_dirs {
        for file in [".mcp.json", "mcp.json"] {
            if dir.join(file).exists() {
                return Err(format!(
                    "the plugin {} brings an MCP server ({file}), and a plugin's MCP server cannot be limited",
                    dir.display()
                ));
            }
        }
        if plugin_manifest_has_mcp(dir) {
            return Err(format!(
                "the plugin {} declares MCP servers, and a plugin's MCP server cannot be limited",
                dir.display()
            ));
        }
    }
    let mut here = Some(cwd);
    while let Some(dir) = here {
        for rel in PROJECT_MCP_FILES {
            if dir.join(rel).exists() {
                return Err(format!(
                    "OMP would load MCP servers from {}, and they cannot be limited",
                    dir.join(rel).display()
                ));
            }
        }
        here = dir.parent();
    }
    if let Some(home) = home {
        for rel in USER_MCP_FILES {
            if home.join(rel).exists() {
                return Err(format!(
                    "OMP would load MCP servers from {}, and they cannot be limited",
                    home.join(rel).display()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_tree_passes_and_each_mcp_file_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("a/b");
        std::fs::create_dir_all(&work).unwrap();
        assert!(check(&work, &[], Some(dir.path())).is_ok());
        std::fs::write(dir.path().join("a/.mcp.json"), "{}").unwrap();
        assert!(check(&work, &[], None).unwrap_err().contains(".mcp.json"));
    }

    #[test]
    fn a_plugin_with_mcp_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("plug");
        std::fs::create_dir_all(p.join(".claude-plugin")).unwrap();
        std::fs::write(
            p.join(".claude-plugin/plugin.json"),
            r#"{"name":"x","mcpServers":{}}"#,
        )
        .unwrap();
        assert!(
            check(dir.path(), std::slice::from_ref(&p), None)
                .unwrap_err()
                .contains("declares MCP")
        );
        std::fs::write(p.join(".claude-plugin/plugin.json"), r#"{"name":"x"}"#).unwrap();
        assert!(check(dir.path(), &[p], None).is_ok());
    }
}
