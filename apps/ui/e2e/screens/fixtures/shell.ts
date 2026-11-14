// The d25 test data of the shell: the pairing, the status, the server's name and this device, the session
// list the sidebar and the Projects cards read, and the run list of the sidebar's Workflow runs group.
// The pairing has the shape the Mac gives the UI at start (r1.s6.w1): this device and the address, with no
// name. The name comes from the `status` answer, as on the Mac.
// Owner: w2. A round-3 item changes a value here only through `overlay` (w5: the active run's `project_id`).
import type { ApiStatus, RunList, SessionList } from "@pulse-factory/api-client";
import { ADDRESS, SERVER, device, runSummary } from "../../../src/test/fake-transport";
import type { PairedServer } from "../../../src/transport/types";
import { AGENT, RUNTIME } from "./agents";
import { S1, S2 } from "./chat";
import type { Verbs } from "./verbs";

export const SERVER_NAME = "draco-desk";
export const THIS_DEVICE = { ...device("D-1", "MacBook"), paired_at: "2026-11-08T09:00:00Z", last_seen_at: "2026-11-08T14:46:00Z" };

export const PAIRED: PairedServer = { address: ADDRESS, device: THIS_DEVICE };

export const STATUS: ApiStatus = {
  schema_version: 1,
  version: "0.1.0",
  key_spki_sha256: "b7".repeat(32),
  device: THIS_DEVICE,
  server: { ...SERVER, name: SERVER_NAME },
};

export const SESSION_LIST: SessionList = {
  schema_version: 1,
  server: { ...SERVER, name: SERVER_NAME },
  runtimes: [RUNTIME],
  agents: [AGENT],
  sessions: [S1, S2],
};

export const RUNS: RunList = {
  schema_version: 1,
  runs: [
    runSummary("r0-s18-1", { spine: "r1.s4", state: "ended", end_reason: "the spine merged" }),
    runSummary("r0-s19-1", { spine: "r1.s5", state: "active", open_requests: 2, waiting: "you answer R-5 and ack R-6" }),
  ],
};

export const UNREACHABLE_MESSAGE = `cannot reach ${ADDRESS}`;

export const verbs: Verbs = {
  session_list: () => SESSION_LIST,
  run_list: () => RUNS,
};
