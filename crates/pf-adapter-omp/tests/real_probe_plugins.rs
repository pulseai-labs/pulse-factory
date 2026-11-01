//! Real probe: what a seat gets under `--no-extensions --no-rules --plugin-dir A --skills=alpha-*`.
//!
//! It settles two claims of ADR-0011 on one real session: whether `--plugin-dir` with `--skills`
//! gives exactly the declared plugin's skills, and which hooks and tools a plugin brings. The
//! first run (2026-11-01, OMP 18.11.3) also had an MCP server in plugin A and in the cwd; both
//! started and both tools reached the seat, so the MCP claims moved to `real_probe_mcp.rs` and
//! to the adapter's refusal (`isolation.rs`). This version has no MCP server, so the launch
//! succeeds and the skill and hook results stay reproducible.
//!
//! `#[ignore]`d. It runs only with `PF_REAL_OMP=1` and spends one real session.
//! `PF_REAL_OMP=1 cargo test -p pf-adapter-omp --test real_probe_plugins -- --ignored --nocapture`

mod common;

use std::path::Path;
use std::time::Duration;

use common::*;
use pf_adapter_omp::probe::{self, Probe};
use pf_engine::port::agent::AgentPort;

const CANARY_MCP: &str = r#"// A tiny MCP server over stdio. It writes a marker file when it starts, so the test sees a start
// even when OMP does not show the server's tools to the model.
const fs = require('node:fs')
const tag = process.argv[2]
fs.writeFileSync(process.argv[3], 'started ' + tag + '\n')
let buf = ''
process.stdin.on('data', (d) => {
  buf += d
  let i
  while ((i = buf.indexOf('\n')) >= 0) {
    const line = buf.slice(0, i); buf = buf.slice(i + 1)
    if (!line.trim()) continue
    const m = JSON.parse(line)
    const reply = (result) => process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: m.id, result }) + '\n')
    if (m.method === 'initialize') reply({ protocolVersion: m.params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: 'canary-' + tag, version: '1.0.0' } })
    else if (m.method === 'tools/list') reply({ tools: [{ name: 'canary_ping_' + tag, description: 'canary', inputSchema: { type: 'object', properties: {} } }] })
    else if (m.method === 'tools/call') reply({ content: [{ type: 'text', text: 'pong' }] })
    else if (m.id !== undefined) reply({})
  }
})
"#;

fn skill(root: &Path, name: &str) {
    let d = root.join("skills").join(name);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Probe skill {name}. Use never.\n---\n\nBody of {name}.\n"),
    )
    .unwrap();
}

fn plugin(root: &Path, name: &str, mcp: Option<(&str, &Path, &Path)>, hook_marker: Option<&Path>) {
    std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
    std::fs::write(
        root.join(".claude-plugin/plugin.json"),
        format!("{{\"name\":\"{name}\",\"version\":\"1.0.0\",\"description\":\"probe\"}}"),
    )
    .unwrap();
    if let Some((tag, script, marker)) = mcp {
        std::fs::write(
            root.join(".mcp.json"),
            format!(
                "{{\"mcpServers\":{{\"canary-{tag}\":{{\"command\":\"node\",\"args\":[\"{}\",\"{tag}\",\"{}\"]}}}}}}",
                script.display(),
                marker.display()
            ),
        )
        .unwrap();
    }
    if let Some(marker) = hook_marker {
        std::fs::create_dir_all(root.join("hooks")).unwrap();
        let cmd = format!("touch {}", marker.display());
        std::fs::write(
            root.join("hooks/hooks.json"),
            format!(
                "{{\"hooks\":{{\"SessionStart\":[{{\"hooks\":[{{\"type\":\"command\",\"command\":\"{cmd}\"}}]}}],\"PreToolUse\":[{{\"matcher\":\"*\",\"hooks\":[{{\"type\":\"command\",\"command\":\"{cmd}\"}}]}}]}}}}"
            ),
        )
        .unwrap();
    }
}

#[test]
#[ignore = "spends a real session; runs only with PF_REAL_OMP=1"]
fn plugin_dir_with_skills_mcp_and_hooks() {
    if !probe::enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    let script = root.join("canary.js");
    std::fs::write(&script, CANARY_MCP).unwrap();

    // Plugin A is declared: two skills, an MCP server and hooks. Plugin B is never passed.
    let a_dir = root.join("plugins/plugin-a");
    let b_dir = root.join("plugins/plugin-b");
    let mcp_plugin = root.join("marker-mcp-plugin");
    let hook_marker = root.join("marker-hook-plugin");
    plugin(&a_dir, "plugin-a", None, Some(&hook_marker));
    skill(&a_dir, "alpha-one");
    skill(&a_dir, "alpha-two");
    plugin(&b_dir, "plugin-b", None, None);
    skill(&b_dir, "beta-one");

    // The seat's cwd holds a project skill, as a worktree can.
    let cwd = root.join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    skill(&cwd.join(".claude"), "gamma-project");
    let _ = (&script, &mcp_plugin);

    let adapter = real_adapter(dir.path());
    let plan = probe_plan(
        dir.path(),
        &cwd,
        "Run the bash command `echo plugin-probe` once. Then reply with the single word DONE.",
        &[("plugins", a_dir.to_str().unwrap()), ("skills", "alpha-*")],
    );
    let Probe::Started(s) = probe::launch(&adapter, &plan, 0).expect("the probe starts") else {
        return;
    };
    let ended = wait_until(Duration::from_secs(120), || {
        adapter.status(&s).unwrap().turn_ended
    });
    std::thread::sleep(Duration::from_secs(2));
    let sdir = pf_adapter_omp::session_dir_of(&adapter, &s).unwrap();
    let prompt = std::fs::read_to_string(sdir.join("probe-system-prompt.txt")).unwrap_or_default();
    let events = std::fs::read_to_string(adapter.event_file(&s).unwrap()).unwrap();
    let inventory = events
        .lines()
        .find(|l| l.contains("\"ev\":\"inventory\""))
        .unwrap_or("(no inventory)")
        .to_string();
    let has = |needle: &str| prompt.contains(needle);
    report("plugins.turn_ended", &ended.to_string());
    report("plugins.inventory", &inventory);
    report("plugins.skill.alpha-one", &has("alpha-one").to_string());
    report("plugins.skill.alpha-two", &has("alpha-two").to_string());
    report(
        "plugins.skill.beta-one(undeclared plugin)",
        &has("beta-one").to_string(),
    );
    report(
        "plugins.skill.gamma-project(cwd .claude/skills)",
        &has("gamma-project").to_string(),
    );
    for name in [
        "brainstorming",
        "archify",
        "memory-recall",
        "using-superpowers",
    ] {
        report(
            &format!("plugins.skill.{name}(operator user skill)"),
            &has(name).to_string(),
        );
    }
    report("plugins.hook.plugin_ran", &hook_marker.exists().to_string());
    let skills_start = prompt
        .find("<skills>")
        .or_else(|| prompt.find("skills"))
        .unwrap_or(0);
    report(
        "plugins.prompt.skills_section",
        &prompt[skills_start..prompt.len().min(skills_start + 1500)].replace('\n', " / "),
    );
    adapter.stop(&s).unwrap();
}
