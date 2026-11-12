// The d25 test data of the Settings page. Owner: w4.
import type { SettingsView } from "@pulse-factory/api-client";
import type { Verbs } from "./verbs";

const SERVER_SCOPE = { kind: "server", project_id: null, agent_id: null } as const;
const BUILT_IN_SCOPE = { kind: "built_in", project_id: null, agent_id: null } as const;

export const SETTINGS: SettingsView = {
  schema_version: 1,
  project_id: null,
  agent_id: null,
  settings: [
    { key: "chat.deny_commands", value: ["rm -rf", "git push --force"], from: SERVER_SCOPE, built_in: ["rm -rf"], merge: "override", applies: "next_launch",
      layers: [{ scope: SERVER_SCOPE, value: ["rm -rf", "git push --force"], set_by: "cli", set_at: "2026-11-08T09:30:00Z" }], masked: [] },
    { key: "chat.propose_per_min", value: 6, from: BUILT_IN_SCOPE, built_in: 6, merge: "override", applies: "next_turn", layers: [], masked: [] },
    { key: "chat.self_turn_cap", value: 4, from: BUILT_IN_SCOPE, built_in: 4, merge: "override", applies: "next_turn", layers: [], masked: [] },
  ],
};

export const verbs: Verbs = {
  settings_get: () => SETTINGS,
};
