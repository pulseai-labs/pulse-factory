// AC-1 of `r1.s5.w2` (Approach 12, G3): the display names of a model, an effort and a permission mode.
// Every row of each table is checked, and so is the pass-through: a value no table holds comes back as it is.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { effortName, modelName, permissionName, runtimeName } from "../ui";

describe("modelName", () => {
  it.each([
    ["claude-opus-5-5", "Opus 5.5"],
    ["claude-sonnet-5-5", "Sonnet 5.5"],
    ["claude-haiku-5-5", "Haiku 5.5"],
    ["claude-fable-5-1", "Fable 5.1"],
    ["claude-fable-5-2", "Fable 5.2"],
    ["claude-opus-4-1", "Opus 4.1"],
  ])("%s is %s", (id, name) => {
    expect(modelName(id)).toBe(name);
  });

  it.each(["fake-model", "claude-opus", "gpt-5", "", "claude-opus-5-5-20260101"])("returns %j unchanged", (id) => {
    expect(modelName(id)).toBe(id);
  });
});

describe("effortName", () => {
  it.each([
    ["low", "Low"],
    ["medium", "Medium"],
    ["high", "High"],
    ["xhigh", "XHigh"],
    ["max", "Max"],
  ])("%s is %s", (effort, name) => {
    expect(effortName(effort)).toBe(name);
  });

  it.each(["", "ultra", "High"])("returns %j unchanged", (effort) => {
    expect(effortName(effort)).toBe(effort);
  });
});

describe("permissionName", () => {
  it.each([
    ["manual", "Ask before edits"],
    ["acceptEdits", "Edit automatically"],
    ["auto", "Auto"],
    ["dontAsk", "Don't ask"],
    ["plan", "Plan mode"],
    ["bypassPermissions", "Bypass permissions"],
  ])("%s is %s", (mode, name) => {
    expect(permissionName(mode)).toBe(name);
  });

  it.each(["bypass", "", "Plan", "default"])("returns %j unchanged", (mode) => {
    expect(permissionName(mode)).toBe(mode);
  });

  it("names every mode of the pinned CLI (crates/pf-adapter-claude/src/launch.rs, MODES)", () => {
    const launch = readFileSync(join(__dirname, "..", "..", "..", "..", "crates", "pf-adapter-claude", "src", "launch.rs"), "utf8");
    const block = /MODES[^=]*=\s*&?\[([^\]]*)\]/.exec(launch)?.[1] ?? "";
    const modes = [...block.matchAll(/"([A-Za-z]+)"/g)].map((m) => m[1] ?? "");
    expect(modes.length).toBe(6);
    for (const mode of modes) expect(permissionName(mode), mode).not.toBe(mode);
  });
});

describe("runtimeName (r1.s6.w3, d17-12)", () => {
  it.each([
    ["claude-code", "Claude Code"],
    ["omp", "OMP"],
  ])("maps %s to %s", (id, name) => {
    expect(runtimeName(id)).toBe(name);
  });

  it.each(["mystery", "Claude-Code", "", "toString", "constructor"])("returns the id %j unchanged", (id) => {
    expect(runtimeName(id)).toBe(id);
  });
});

describe("the names are plain functions", () => {
  it("the file calls nothing and reads no state", () => {
    const text = readFileSync(join(__dirname, "..", "ui", "display-names.ts"), "utf8");
    for (const word of ["useState", "useContext", "fetch(", "setTimeout", "localStorage"]) {
      expect(text.includes(word), word).toBe(false);
    }
  });
});
