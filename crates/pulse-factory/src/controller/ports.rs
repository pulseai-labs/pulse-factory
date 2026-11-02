//! The ports a run needs: its agent adapters and its forge. A trait, so a test can give a
//! `FakeAdapter` and a `BareForge` where the live controller gives OMP and GitHub.

use std::path::PathBuf;
use std::sync::Arc;

use pf_engine::port::agent::{AgentPort, AgentRouter};
use pf_engine::port::forge::ForgePort;

/// What the ports of one run are built from.
#[derive(Clone, Debug)]
pub struct RunSpec {
    pub run_id: String,
    pub run_dir: PathBuf,
    /// The runtime names the workflow's roles use.
    pub runtimes: Vec<String>,
    pub plugin_roots: Vec<PathBuf>,
    pub paper: Option<PathBuf>,
    pub forge_repo: String,
}

/// The OMP adapter's config for a run: its sessions under the run directory, the plugin roots of
/// the run, and the run's own copy of the package for briefs.
pub fn omp_config(spec: &RunSpec) -> pf_adapter_omp::OmpConfig {
    let mut cfg = pf_adapter_omp::OmpConfig::new(spec.run_dir.join("omp"));
    cfg.repo = spec.forge_repo.clone();
    cfg.brief_root = Some(spec.run_dir.join("package"));
    cfg.plugin_roots = spec.plugin_roots.clone();
    cfg
}

/// The ports of one run.
pub struct RunPorts {
    pub agent: Arc<dyn AgentPort>,
    pub forge: Arc<dyn ForgePort>,
    /// The host the seat's sandbox may reach to talk to the forge. Never empty.
    pub forge_api_hosts: String,
}

pub trait Ports: Send + Sync {
    fn build(&self, spec: &RunSpec) -> Result<RunPorts, String>;
}

/// The live ports: OMP and GitHub, or the paper env with `--paper`.
pub struct LivePorts;

impl Ports for LivePorts {
    fn build(&self, spec: &RunSpec) -> Result<RunPorts, String> {
        let mut router = AgentRouter::new();
        if let Some(paper) = &spec.paper {
            let adapter = Arc::new(pf_adapter_fake::PaperAdapter::new(
                pf_adapter_fake::PaperConfig::new(paper.clone(), spec.plugin_roots.clone()),
            ));
            let mut names = spec.runtimes.clone();
            names.sort();
            names.dedup();
            for name in names {
                router
                    .register(&name, adapter.clone())
                    .map_err(|e| e.to_string())?;
            }
            let forge = pf_forge_bare::BareForge::open(paper.join("forge.git"))
                .map_err(|e| format!("cannot open the paper env's forge: {e}"))?;
            return Ok(RunPorts {
                agent: Arc::new(router),
                forge: Arc::new(forge),
                forge_api_hosts: "forge.invalid".to_string(),
            });
        }
        router
            .register(
                "omp",
                Arc::new(pf_adapter_omp::OmpAdapter::new(omp_config(spec))),
            )
            .map_err(|e| e.to_string())?;
        let forge = pf_forge_github::GithubForge::new(&spec.forge_repo)
            .map_err(|e| format!("cannot use the GitHub repo {}: {e}", spec.forge_repo))?;
        Ok(RunPorts {
            agent: Arc::new(router),
            forge: Arc::new(forge),
            forge_api_hosts: "api.github.com".to_string(),
        })
    }
}
