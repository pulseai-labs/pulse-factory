// The names the screens show for a model, an effort, a permission mode and a runtime (G3). Four pure functions over
// plain tables: they make no call and read no state. Every screen uses these names and none keeps its
// own. A value that no table holds comes back unchanged, so the app never hides a value the server sent.

const FAMILIES: Readonly<Record<string, string>> = {
  opus: "Opus",
  sonnet: "Sonnet",
  haiku: "Haiku",
  fable: "Fable",
};

/** `claude-opus-5-5` is "Opus 5.5". An id that is not `claude-<family>-<major>-<minor>` comes back as it is. */
export function modelName(id: string): string {
  const match = /^claude-([a-z]+)-(\d+)-(\d+)$/.exec(id);
  const family = match?.[1] === undefined ? undefined : FAMILIES[match[1]];
  return match === null || family === undefined ? id : `${family} ${match[2]}.${match[3]}`;
}

const EFFORTS: Readonly<Record<string, string>> = {
  low: "Low",
  medium: "Medium",
  high: "High",
  xhigh: "XHigh",
  max: "Max",
};

export function effortName(effort: string): string {
  return Object.hasOwn(EFFORTS, effort) ? (EFFORTS[effort] as string) : effort;
}

/** The six modes of the pinned CLI (`crates/pf-adapter-claude/src/launch.rs`, `MODES`). */
const PERMISSIONS: Readonly<Record<string, string>> = {
  manual: "Ask before edits",
  acceptEdits: "Edit automatically",
  auto: "Auto",
  dontAsk: "Don't ask",
  plan: "Plan mode",
  bypassPermissions: "Bypass permissions",
};

export function permissionName(mode: string): string {
  return Object.hasOwn(PERMISSIONS, mode) ? (PERMISSIONS[mode] as string) : mode;
}

const RUNTIMES: Readonly<Record<string, string>> = {
  "claude-code": "Claude Code",
  omp: "OMP",
};

/** The name of a runtime id as 11g shows it ("claude-code" is "Claude Code"). An id the table does not hold comes back as it is. */
export function runtimeName(id: string): string {
  return Object.hasOwn(RUNTIMES, id) ? (RUNTIMES[id] as string) : id;
}
