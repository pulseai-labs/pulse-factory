// The d25 test data of the Agents page: the runtime and the agent record that `session_list` carries.
// Owner: w4. The shell's `SESSION_LIST` (fixtures/shell.ts) and the chat's composer read them.
import { claudeRuntime, orchestrator } from "../../../src/test/fake-transport";
import type { Verbs } from "./verbs";

export const RUNTIME = claudeRuntime([], {
  options: [
    { id: "model", label: "Model", values: ["claude-opus-5-5", "claude-sonnet-5-5"], default: "claude-opus-5-5", free: true, live: true },
    { id: "effort", label: "Effort", values: ["low", "medium", "high", "max"], default: "high", free: false, live: true },
    { id: "permission_mode", label: "Permission mode", values: ["manual", "plan", "bypass"], default: "bypass", free: false, live: true },
  ],
});

export const AGENT = orchestrator({
  title: "Plans spines with you and arms workflows",
  model: "claude-opus-5-5",
  skills: ["pulse-factory", "ossify"],
  runtime_options: {},
});

/** No verb of its own: the agent and the runtime come in `session_list` (fixtures/shell.ts). */
export const verbs: Verbs = {};
