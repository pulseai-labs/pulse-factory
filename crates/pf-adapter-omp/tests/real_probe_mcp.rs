//! Real probe: does the extension keep a project's MCP server tools away from the seat?
//!
//! OMP started the MCP servers of a project `.mcp.json` and a declared plugin under the flags,
//! and `--tools` did not hide their tools (`real_probe_plugins`). The extension now calls
//! `pi.setActiveTools` with the seat tool list. This probe launches a seat in a directory with a
//! project MCP config, with the adapter's static refusal off, and reports what the seat holds.
//!
//! `#[ignore]`d. It runs only with `PF_REAL_OMP=1` and spends one real session.
//! `PF_REAL_OMP=1 cargo test -p pf-adapter-omp --test real_probe_mcp -- --ignored --nocapture`

mod common;

use common::*;
use pf_adapter_omp::probe::{self, Probe};

#[test]
#[ignore = "spends a real session; runs only with PF_REAL_OMP=1"]
fn project_mcp_tools_do_not_reach_the_seat() {
    if !probe::enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    let marker = root.join("marker-mcp-project");
    let script = root.join("canary.js");
    std::fs::write(
        &script,
        "const fs=require('node:fs');fs.writeFileSync(process.argv[3],'started\\n');let b='';process.stdin.on('data',d=>{b+=d;let i;while((i=b.indexOf('\\n'))>=0){const l=b.slice(0,i);b=b.slice(i+1);if(!l.trim())continue;const m=JSON.parse(l);const r=x=>process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:m.id,result:x})+'\\n');if(m.method==='initialize')r({protocolVersion:m.params.protocolVersion,capabilities:{tools:{}},serverInfo:{name:'canary',version:'1'}});else if(m.method==='tools/list')r({tools:[{name:'canary_ping',description:'c',inputSchema:{type:'object',properties:{}}}]});else if(m.id!==undefined)r({content:[{type:'text',text:'pong'}]})}})",
    )
    .unwrap();
    let cwd = root.join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(
        cwd.join(".mcp.json"),
        format!(
            "{{\"mcpServers\":{{\"canary\":{{\"command\":\"node\",\"args\":[\"{}\",\"x\",\"{}\"]}}}}}}",
            script.display(),
            marker.display()
        ),
    )
    .unwrap();
    let adapter = real_adapter(dir.path());
    let plan = probe_plan(
        dir.path(),
        &cwd,
        "Run the bash command `echo mcp-probe` once. Then reply with the single word DONE.",
        &[],
    );
    // Result of the run on 2026-11-01 (OMP 18.11.3): `pi.setActiveTools` with the seat list did
    // not remove the MCP tool, so the adapter ended the launch with `Refused`. Anything else
    // means OMP changed: qualify the new version again.
    match probe::launch(&adapter, &plan, 0) {
        Err(why) => {
            report("mcp.launch", &why);
            assert!(
                why.contains("cannot be limited") && why.contains("mcp__canary_ping"),
                "{why}"
            );
            report("mcp.server_started", &marker.exists().to_string());
        }
        Ok(Probe::Skipped) => {}
        Ok(Probe::Started(s)) => {
            let events = std::fs::read_to_string(adapter.event_file(&s).unwrap()).unwrap();
            report(
                "mcp.inventory",
                events
                    .lines()
                    .find(|l| l.contains("inventory"))
                    .unwrap_or("none"),
            );
            adapter.stop(&s).unwrap();
            panic!(
                "the project MCP tool no longer reaches the seat: OMP changed, and the adapter can drop the refusal"
            );
        }
    }
}
