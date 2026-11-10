//! The agents: reads over the store for the persona and the launch settings (ADR-0015). An agent is
//! a row. The built-in Orchestrator is seeded by migration 0005. There is no create or edit here:
//! custom agents are Release 2's.
//!
//! The built-in Orchestrator's instructions are NULL in its row, so the binary holds the text. The
//! controller passes it as `ChatOpen.instructions`. It is the same for every process of one
//! session, so it names only stable facts and holds no time.

use std::path::Path;

use pf_engine::port::store::{AgentRow, ORCHESTRATOR_AGENT, Store};

use super::Controller;

impl Controller {
    /// Every agent, in creation order.
    pub fn agents(&self) -> Result<Vec<AgentRow>, String> {
        self.store.agents().map_err(|e| e.to_string())
    }

    /// One agent by ID.
    pub fn agent(&self, id: &str) -> Result<Option<AgentRow>, String> {
        self.store.agent(id).map_err(|e| e.to_string())
    }
}

/// The instructions an agent's chat process gets. A built-in agent's text is in the binary, and a
/// stored text (a later release's custom agent) is used as it is. `share_dir` is the server's real
/// share directory, where `scripts/install.sh` puts the skill.
pub fn instructions(
    agent: &AgentRow,
    project_name: &str,
    root: &str,
    server_name: &str,
    session_id: &str,
    share_dir: &Path,
) -> String {
    if let Some(text) = &agent.instructions {
        return text.clone();
    }
    if agent.id == ORCHESTRATOR_AGENT {
        return orchestrator_persona(project_name, root, server_name, session_id, share_dir);
    }
    String::new()
}

/// The built-in Orchestrator's persona. It names the project, the root, the server and the session
/// and the skill's path under the share directory, and nothing that changes. It names no plugin, no
/// hook and no role of any seat.
pub fn orchestrator_persona(
    project_name: &str,
    root: &str,
    server_name: &str,
    session_id: &str,
    share_dir: &Path,
) -> String {
    let skill = share_dir.join("skill").join("SKILL.md");
    let skill = skill.display();
    format!(
        "You are the Orchestrator of the project {project_name} in Pulse Factory. You run in the \
         project's main worktree, {root}, on the server {server_name}. This chat is session \
         {session_id}.\n\
         \n\
         The operator talks to you from the Pulse Factory app or from the command line. Pulse \
         Factory is the factory that runs the project's work. These commands read it: \
         `pulse-factory status`, `pulse-factory requests`, `pulse-factory show` and \
         `pulse-factory project list`. The skill at {skill} describes every \
         verb.\n\
         \n\
         To start a planned piece of work, never run pulse-factory arm: run pulse-factory propose \
         with the spine and the workflow inputs the skill lists. It posts a start card into this \
         chat and starts nothing; the operator confirms it on a paired device.\n\
         \n\
         Nothing but an operator message, or a background task that you started, begins a turn. \
         Never start a timer, a loop or a poll to wake yourself. When you have nothing to do, \
         end your turn and wait for the operator.\n"
    )
}
