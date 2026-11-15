// The body of the sidebar on the Agents page (11g, r1.s5.w4): the head "Agents" with New agent `+` as `NotBuilt`
// and its "Later release" tag beside it (r1.s6.w3, NG-19), and one row for each agent the server lists: the
// avatar (28 px), the name, the agent's second line and an amber count of its live sessions that need you. The
// Orchestrator's second line is "One session per project" (the server's rule, `sessions.rs` `live_orchestrator`,
// NG-21); any other agent's is its title. A click selects the agent (`screens/agentSelection.ts`); the
// selected row has `aria-current="true"`. The list shows with one agent too. It makes no call: the agents are
// in the session list the page reads.
import { isOrchestrator, selectAgent, useSelectedAgent } from "../screens/agentSelection";
import { useChat } from "../state/chat";
import { Avatar, CountBadge, LaterTag, NotBuilt, agentLook } from "../ui";

export function AgentsSidebar() {
  const chat = useChat();
  const agents = chat.list?.agents ?? [];
  const current = useSelectedAgent(agents);
  return (
    <>
      <div className="sb-head">
        <span className="sb-title">Agents</span>
        <span className="sb-new">
          <NotBuilt as="button" compact icon="plus" label="New agent" />
          <span aria-hidden="true">
            <LaterTag />
          </span>
        </span>
      </div>
      <ul className="ag-list" aria-label="Agent list">
        {agents.map((a) => {
          const waiting = chat.live.filter((s) => s.agent_id === a.id && s.state === "needs_you").length;
          return (
            <li key={a.id}>
              <button
                type="button"
                className={`ag-row${current?.id === a.id ? " ag-row-on" : ""}`}
                aria-label={a.name}
                aria-current={current?.id === a.id ? "true" : undefined}
                onClick={() => selectAgent(a.id)}
              >
                <Avatar agent={agentLook(a.id)} size={28} />
                <span className="ag-row-text">
                  <span className="ag-row-name">{a.name}</span>
                  <span className="ag-row-title">{isOrchestrator(a) ? "One session per project" : a.title}</span>
                </span>
                <CountBadge count={waiting} title={`${waiting} waiting`} />
              </button>
            </li>
          );
        })}
      </ul>
    </>
  );
}
