// The test data of d25: what the fixture server answers for the screens. It is typed with the
// `@pulse-factory/api-client` types and built with the unit tests' builders (`src/test/fake-transport.ts`),
// so `npm run typecheck` checks it against the wire types. It is byte-stable: fixed IDs, fixed times on
// 2026-11-08 between 09:00 and 14:47 UTC, fixed costs and SHAs. It holds no token and no pairing code.
//
// The data lives in one module per area, `fixtures/<area>.ts`, so that each round-3 item edits only its own:
// shell (w2), chat (w3), needs-you, agents and settings (w4), projects (w5). Each module exports `verbs`, the
// answers of its verbs. This file re-exports them, so `server.ts` and the spec keep one import. A value
// that another screen shows is changed only with that screen's screenshot, in the same PR.
export { AGENT, RUNTIME, verbs as agentsVerbs } from "./fixtures/agents";
export { S1, S2, S2_ITEMS, streamEvents, verbs as chatVerbs } from "./fixtures/chat";
export { DETAILS, REQUESTS, REQUESTS_FRAME, REQUEST_LIST, verbs as needsYouVerbs } from "./fixtures/needs-you";
export { PROJECTS, verbs as projectsVerbs } from "./fixtures/projects";
export { SETTINGS, verbs as settingsVerbs } from "./fixtures/settings";
export { PAIRED, RUNS, SERVER_NAME, SESSION_LIST, STATUS, THIS_DEVICE, UNREACHABLE_MESSAGE, verbs as shellVerbs } from "./fixtures/shell";
