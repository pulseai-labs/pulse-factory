//! The agents: reads over the store for the persona and the launch settings (ADR-0015). An agent is
//! a row. The built-in Orchestrator is seeded by migration 0005. There is no create or edit here:
//! custom agents are Release 2's.

use pf_engine::port::store::{AgentRow, Store};

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
