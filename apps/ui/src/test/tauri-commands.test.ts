// The Tauri commands of the Mac app, checked without building it (the app builds on GitHub's macOS
// runner only). This test reads the shell's `main.rs` and the contract from disk, and checks that
// four lists name the same commands: `generate_handler![…]`, the `#[tauri::command]` functions,
// `TAURI_COMMANDS` of `tauri.ts` and the commands table of `docs/contracts/ui-transport.md`. It also
// checks that each command's Rust argument names are the `invoke` keys of `tauri.ts` in snake case.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { TAURI_COMMANDS } from "../transport/tauri";

function read(relative: string): string {
  return readFileSync(fileURLToPath(new URL(relative, import.meta.url)), "utf8");
}

const MAIN_RS = read("../../../desktop/src-tauri/src/main.rs");
const TAURI_TS = read("../transport/tauri.ts");
const CONTRACT = read("../../../../docs/contracts/ui-transport.md");

function snake(name: string): string {
  return name.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`);
}

/** The names in `tauri::generate_handler![ … ]`. */
function handlerNames(): string[] {
  const match = /generate_handler!\[([^\]]*)\]/.exec(MAIN_RS);
  expect(match, "main.rs has no generate_handler![…]").not.toBeNull();
  return (match?.[1] ?? "")
    .split(",")
    .map((n) => n.trim())
    .filter((n) => n !== "");
}

interface RustCommand {
  name: string;
  /** The arguments the webview sends: every argument except the managed `State`. */
  args: Array<{ name: string; type: string }>;
}

/** Each function that follows a `#[tauri::command]` attribute, with its arguments. */
function rustCommands(): RustCommand[] {
  const out: RustCommand[] = [];
  const pattern = /#\[tauri::command\]\s*async fn (\w+)\s*\(([^)]*)\)/g;
  for (const m of MAIN_RS.matchAll(pattern)) {
    const args = (m[2] ?? "")
      .split(/,\s*(?![^<]*>)/)
      .map((a) => a.trim())
      .filter((a) => a !== "")
      .map((a) => {
        const [name, ...type] = a.split(":");
        return { name: (name ?? "").trim(), type: type.join(":").trim() };
      })
      .filter((a) => !a.type.startsWith("State<"));
    out.push({ name: m[1] ?? "", args });
  }
  return out;
}

/** The names in the commands table of the contract: the rows of "## The Tauri commands". */
function contractCommands(): string[] {
  const start = CONTRACT.indexOf("## The Tauri commands");
  expect(start, "the contract has no 'The Tauri commands' section").toBeGreaterThan(-1);
  const rest = CONTRACT.slice(start + 1);
  const end = rest.search(/\n## /);
  const section = end === -1 ? rest : rest.slice(0, end);
  return [...section.matchAll(/^\| `(\w+)` \|/gm)].map((m) => m[1] ?? "");
}

/** The keys each command passes to `invoke` in `tauri.ts`: `command<T>("name", { a, b })`. */
function invokeKeys(): Map<string, string[]> {
  const keys = new Map<string, string[]>();
  const pattern = /command(?:<[^>]*>)?\(\s*"(\w+)"\s*,\s*(undefined|\{([^}]*)\})/g;
  for (const m of TAURI_TS.matchAll(pattern)) {
    const list = (m[3] ?? "")
      .split(",")
      .map((k) => (k.split(":")[0] ?? "").trim())
      .filter((k) => k !== "");
    keys.set(m[1] ?? "", list);
  }
  return keys;
}

describe("the Tauri commands", () => {
  it("are the same set in the handler list, the functions, tauri.ts and the contract", () => {
    const handlers = [...handlerNames()].sort();
    const functions = rustCommands()
      .map((c) => c.name)
      .sort();
    const typescript = [...TAURI_COMMANDS].sort();
    const contract = [...contractCommands()].sort();
    expect(handlers.length).toBeGreaterThanOrEqual(15);
    expect(functions).toEqual(handlers);
    expect(typescript).toEqual(handlers);
    expect(contract).toEqual(handlers);
    expect(new Set(handlers).size).toBe(handlers.length);
  });

  it("name the nine commands of the chat", () => {
    for (const name of [
      "session_list",
      "session_start",
      "session_send",
      "session_answer",
      "session_interrupt",
      "session_set",
      "session_end",
      "open_stream",
      "close_stream",
    ]) {
      expect(TAURI_COMMANDS, name).toContain(name);
    }
  });

  it("take the arguments that tauri.ts sends, in snake case", () => {
    const keys = invokeKeys();
    for (const command of rustCommands()) {
      const sent = keys.get(command.name);
      expect(sent, `tauri.ts invokes ${command.name}`).toBeDefined();
      expect(
        (sent ?? []).map(snake).sort(),
        `the arguments of ${command.name}`,
      ).toEqual(command.args.map((a) => a.name).sort());
    }
  });

  it("open_stream takes its events on a Channel", () => {
    const open = rustCommands().find((c) => c.name === "open_stream");
    const onEvent = open?.args.find((a) => a.name === "on_event");
    expect(onEvent?.type).toMatch(/^tauri::ipc::Channel<StreamEvent>$/);
    expect(open?.args.find((a) => a.name === "options")?.type).toBe("StreamOptions");
    expect(MAIN_RS).toContain("on_page_load");
    expect(MAIN_RS).toContain("close_all");
  });

  it("add no capability file and no event listener", () => {
    // Tauri's `listen` needs a capability that grants core:event; a Channel needs none.
    const code = TAURI_TS.replace(/\/\/.*$/gm, "");
    expect(code).not.toMatch(/\blisten\b/);
    expect(code).toContain("Channel");
  });
});
