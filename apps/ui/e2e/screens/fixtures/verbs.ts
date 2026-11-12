// The type of the verb table of d25. Each fixture module exports `verbs`: a map from a verb name of the bridge
// protocol to its fixture answer, as a function of the call's arguments (most ignore them). `server.ts` merges
// the six tables and fails at start when a verb is in two of them, naming it.
export type Verbs = Record<string, (args: unknown) => unknown>;
