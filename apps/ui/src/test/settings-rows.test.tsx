// AC-10 (r1.s4.w3): the Chat policies rows on the Settings page. Each key as a chosen project and agent
// read it, the scope its value comes from, the values it masks, and a set or unset at that scope. The
// page names no key: it renders the records `settings_get` returns (W11).
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import type { SettingScope } from "@pulse-factory/api-client";
import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { boot, type User } from "./chat-helpers";
import { FakeTransport, orchestrator, project } from "./fake-transport";

const SERVER: SettingScope = { kind: "server", project_id: null, agent_id: null };
const AT = "2026-11-10T10:00:00.000Z";

const rail = () => within(screen.getByRole("navigation"));
const section = () => within(screen.getByRole("region", { name: "Chat policies" }));
const row = (key: string) => within(screen.getByRole("group", { name: `Setting ${key}` }));

function seeded(): FakeTransport {
  const fake = new FakeTransport({ projects: [project(1, "alpha"), project(2, "beta")] });
  fake.agents = [orchestrator()];
  return fake;
}

async function open(fake: FakeTransport): Promise<User> {
  const user = await boot(fake);
  await user.click(rail().getByRole("button", { name: "Settings" }));
  await screen.findByRole("region", { name: "Chat policies" });
  await screen.findByRole("group", { name: "Setting chat.self_turn_cap" });
  return user;
}

async function choose(user: User, label: "Project" | "Agent", option: string) {
  await user.selectOptions(section().getByLabelText(label), option);
}

describe("the Chat policies section", () => {
  it("reads the settings once when the page opens and shows one row for each key the server returns", async () => {
    const fake = seeded();
    await open(fake);

    expect(fake.settingsGetArgs).toEqual([{}]);
    const rows = section().getAllByRole("group");
    expect(rows.map((r) => r.getAttribute("aria-label"))).toEqual([
      "Setting chat.self_turn_cap",
      "Setting chat.deny_commands",
      "Setting chat.propose_per_min",
    ]);
  });

  it("shows Value, From and Applies for each row, a number as a number and a list one pattern per line", async () => {
    const fake = seeded();
    fake.settingLayers["chat.self_turn_cap"] = [{ scope: SERVER, value: 6, set_by: "cli", set_at: AT }];
    await open(fake);

    const cap = row("chat.self_turn_cap");
    expect(cap.getByText("6")).toBeTruthy();
    expect(cap.getByText("the server default")).toBeTruthy();
    expect(cap.getByText("at the next turn")).toBeTruthy();

    const deny = row("chat.deny_commands");
    expect(deny.getByText("built-in")).toBeTruthy();
    expect(deny.getByText("at the next launch")).toBeTruthy();
    expect(deny.getByText("rm -rf").tagName).toBe("PRE");
  });

  it("shows a list value with one pattern on each line", async () => {
    const fake = seeded();
    fake.settingLayers["chat.deny_commands"] = [
      { scope: SERVER, value: ["git push", "rm -rf /"], set_by: "cli", set_at: AT },
    ];
    await open(fake);
    const value = row("chat.deny_commands").getByText(/git push/);
    expect(value.textContent).toBe("git push\nrm -rf /");
  });

  it("names the scope in words and makes one settingsGet with the context when a select changes", async () => {
    const fake = seeded();
    const user = await open(fake);
    expect(section().getByText("Scope: the server default")).toBeTruthy();

    await choose(user, "Project", "alpha");
    await waitFor(() => expect(fake.settingsGetArgs).toHaveLength(2));
    expect(fake.settingsGetArgs[1]).toEqual({ project_id: "P-1" });
    expect(section().getByText("Scope: project alpha")).toBeTruthy();

    await choose(user, "Agent", "Orchestrator");
    await waitFor(() => expect(fake.settingsGetArgs).toHaveLength(3));
    expect(fake.settingsGetArgs[2]).toEqual({ project_id: "P-1", agent_id: "orchestrator" });
    expect(section().getByText("Scope: agent Orchestrator in project alpha")).toBeTruthy();

    await choose(user, "Project", "All projects");
    await waitFor(() => expect(fake.settingsGetArgs).toHaveLength(4));
    expect(fake.settingsGetArgs[3]).toEqual({ agent_id: "orchestrator" });
    expect(section().getByText("Scope: agent Orchestrator")).toBeTruthy();
  });

  it("offers All projects and each project, All agents and each agent of the server's list", async () => {
    const fake = seeded();
    await open(fake);
    const projects = within(section().getByLabelText("Project")).getAllByRole("option");
    expect(projects.map((o) => o.textContent)).toEqual(["All projects", "alpha", "beta"]);
    await waitFor(() =>
      expect(within(section().getByLabelText("Agent")).getAllByRole("option").map((o) => o.textContent)).toEqual([
        "All agents",
        "Orchestrator",
      ]),
    );
  });

  it("shows From and a `masks` line for each layer the winner hides", async () => {
    const fake = seeded();
    const alpha: SettingScope = { kind: "project", project_id: "P-1", agent_id: null };
    fake.settingLayers["chat.self_turn_cap"] = [
      { scope: SERVER, value: 6, set_by: "cli", set_at: AT },
      { scope: alpha, value: 2, set_by: "D-1", set_at: AT },
    ];
    const user = await open(fake);
    const server = row("chat.self_turn_cap");
    expect(server.getByText("the server default")).toBeTruthy();
    expect(server.queryByText(/^masks /)).toBeNull();

    await choose(user, "Project", "alpha");
    await waitFor(() => expect(row("chat.self_turn_cap").getByText("project alpha")).toBeTruthy());
    const cap = row("chat.self_turn_cap");
    expect(cap.getByText("2")).toBeTruthy();
    expect(cap.getByText("masks the server default 6")).toBeTruthy();
  });

  it("shows no row of an earlier context as the new context's while the new read is out", async () => {
    const fake = seeded();
    fake.settingLayers["chat.self_turn_cap"] = [
      { scope: { kind: "project", project_id: "P-1", agent_id: null }, value: 2, set_by: "D-1", set_at: AT },
    ];
    const user = await open(fake);
    await choose(user, "Project", "alpha");
    await waitFor(() => expect(row("chat.self_turn_cap").getByText("project alpha")).toBeTruthy());

    fake.fail("settingsGet", "server", "the settings are busy");
    await choose(user, "Project", "beta");
    expect(await section().findByRole("alert")).toBeTruthy();
    expect(section().queryByRole("group")).toBeNull();
    expect(section().queryByText("project alpha")).toBeNull();
  });
});

describe("Set and Unset", () => {
  it("sends one settingsSet with the key, the parsed number, the scope's IDs and a command ID, and shows `resolved`", async () => {
    const fake = seeded();
    const user = await open(fake);
    await choose(user, "Project", "alpha");
    await waitFor(() => expect(fake.settingsGetArgs).toHaveLength(2));

    const cap = row("chat.self_turn_cap");
    expect(cap.getByLabelText("New value").getAttribute("type")).toBe("number");
    await user.type(cap.getByLabelText("New value"), "5");
    await user.click(cap.getByRole("button", { name: "Set at this scope" }));

    await waitFor(() => expect(fake.settingsSetArgs).toHaveLength(1));
    const sent = fake.settingsSetArgs[0];
    expect(sent).toMatchObject({ key: "chat.self_turn_cap", value: 5, project_id: "P-1" });
    expect(sent?.agent_id ?? null).toBeNull();
    expect(typeof sent?.command_id).toBe("string");
    expect(sent?.command_id).not.toBe("");

    await waitFor(() => expect(row("chat.self_turn_cap").getByText("project alpha")).toBeTruthy());
    expect(row("chat.self_turn_cap").getByText("5")).toBeTruthy();
    // The answer replaced the row: no further read.
    expect(fake.settingsGetArgs).toHaveLength(2);
  });

  it("splits a list value by line, drops blank lines and trims", async () => {
    const fake = seeded();
    const user = await open(fake);
    const deny = row("chat.deny_commands");
    const field = deny.getByLabelText("New value");
    expect(field.tagName).toBe("TEXTAREA");
    await user.type(field, "git push{Enter}{Enter}  rm -rf /  {Enter}");
    await user.click(deny.getByRole("button", { name: "Set at this scope" }));

    await waitFor(() => expect(fake.settingsSetArgs).toHaveLength(1));
    expect(fake.settingsSetArgs[0]).toMatchObject({ key: "chat.deny_commands", value: ["git push", "rm -rf /"] });
    expect(fake.settingsSetArgs[0]?.project_id ?? null).toBeNull();
  });

  it("sends the project and the agent of the chosen context", async () => {
    const fake = seeded();
    const user = await open(fake);
    await choose(user, "Project", "beta");
    await choose(user, "Agent", "Orchestrator");
    await waitFor(() => expect(fake.settingsGetArgs).toHaveLength(3));
    const cap = row("chat.self_turn_cap");
    await user.type(cap.getByLabelText("New value"), "3");
    await user.click(cap.getByRole("button", { name: "Set at this scope" }));
    await waitFor(() => expect(fake.settingsSetArgs).toHaveLength(1));
    expect(fake.settingsSetArgs[0]).toMatchObject({ key: "chat.self_turn_cap", value: 3, project_id: "P-2", agent_id: "orchestrator" });
  });

  it("does not send a number field that holds no number, and says so", async () => {
    const fake = seeded();
    const user = await open(fake);
    const cap = row("chat.self_turn_cap");
    await user.click(cap.getByRole("button", { name: "Set at this scope" }));
    expect(fake.count("settingsSet")).toBe(0);
    expect(cap.getByText("Enter a number.")).toBeTruthy();
  });

  it("does not send a list field that holds no pattern, and says so: an empty list is an Unset, not a Set", async () => {
    const fake = seeded();
    const user = await open(fake);
    const deny = row("chat.deny_commands");

    // Left blank.
    await user.click(deny.getByRole("button", { name: "Set at this scope" }));
    expect(fake.count("settingsSet")).toBe(0);
    expect(deny.getByRole("alert").textContent).toBe("Enter at least one pattern, or use Unset.");

    // Blank lines and spaces only: still no pattern.
    await user.type(deny.getByLabelText("New value"), "  {Enter}{Enter} {Enter}");
    await user.click(deny.getByRole("button", { name: "Set at this scope" }));
    expect(fake.count("settingsSet")).toBe(0);
    expect(deny.getByRole("alert").textContent).toBe("Enter at least one pattern, or use Unset.");
  });

  it("shows Unset only for a layer at exactly the chosen scope, and unsetting shows the built-in value again", async () => {
    const fake = seeded();
    fake.settingLayers["chat.self_turn_cap"] = [{ scope: SERVER, value: 6, set_by: "cli", set_at: AT }];
    const user = await open(fake);
    // The server scope has its own layer for this key, and none for the others.
    expect(row("chat.self_turn_cap").getByRole("button", { name: "Unset at this scope" })).toBeTruthy();
    expect(row("chat.deny_commands").queryByRole("button", { name: "Unset at this scope" })).toBeNull();

    // The project scope has none: the server layer sits at another scope.
    await choose(user, "Project", "alpha");
    await waitFor(() => expect(row("chat.self_turn_cap").getByText("the server default")).toBeTruthy());
    expect(row("chat.self_turn_cap").queryByRole("button", { name: "Unset at this scope" })).toBeNull();

    // Back at the server scope: unset.
    await choose(user, "Project", "All projects");
    const unset = await waitFor(() => row("chat.self_turn_cap").getByRole("button", { name: "Unset at this scope" }));
    await user.click(unset);
    await waitFor(() => expect(fake.settingsUnsetArgs).toHaveLength(1));
    expect(fake.settingsUnsetArgs[0]).toMatchObject({ key: "chat.self_turn_cap" });
    expect(typeof fake.settingsUnsetArgs[0]?.command_id).toBe("string");
    await waitFor(() => expect(row("chat.self_turn_cap").getByText("built-in")).toBeTruthy());
    expect(row("chat.self_turn_cap").getByText("4")).toBeTruthy();
    expect(row("chat.self_turn_cap").queryByRole("button", { name: "Unset at this scope" })).toBeNull();
  });

  it("names no button Settings", async () => {
    const fake = seeded();
    fake.settingLayers["chat.self_turn_cap"] = [{ scope: SERVER, value: 6, set_by: "cli", set_at: AT }];
    await open(fake);
    expect(within(screen.getByRole("main")).queryAllByRole("button", { name: /settings/i })).toHaveLength(0);
    expect(rail().getAllByRole("button", { name: "Settings" })).toHaveLength(1);
  });
});

describe("a refusal and a lost answer", () => {
  it("shows the server's text on the row for a refusal, and keeps the field", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("settingsSet", "refused", "chat.self_turn_cap must be at least 1", 1);
    const cap = row("chat.self_turn_cap");
    await user.type(cap.getByLabelText("New value"), "0");
    await user.click(cap.getByRole("button", { name: "Set at this scope" }));

    expect(await cap.findByText("chat.self_turn_cap must be at least 1")).toBeTruthy();
    expect(cap.queryByRole("button", { name: "Retry" })).toBeNull();
    expect((cap.getByLabelText("New value") as HTMLInputElement).value).toBe("0");
    // The other rows are untouched.
    expect(within(screen.getByRole("group", { name: "Setting chat.deny_commands" })).queryByRole("alert")).toBeNull();
  });

  it("keeps the command ID after unreachable: Retry sends the same call, and a new Set gets a new ID", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("settingsSet", "unreachable", "no answer", 1);
    const cap = row("chat.self_turn_cap");
    await user.type(cap.getByLabelText("New value"), "5");
    await user.click(cap.getByRole("button", { name: "Set at this scope" }));

    const retry = await cap.findByRole("button", { name: "Retry" });
    expect(fake.settingsSetArgs).toHaveLength(1);
    await user.click(retry);
    await waitFor(() => expect(fake.settingsSetArgs).toHaveLength(2));
    expect(fake.settingsSetArgs[1]).toEqual(fake.settingsSetArgs[0]);
    await waitFor(() => expect(row("chat.self_turn_cap").getByText("the server default")).toBeTruthy());
    expect(row("chat.self_turn_cap").queryByRole("button", { name: "Retry" })).toBeNull();

    // A later action is a new action.
    await user.clear(row("chat.self_turn_cap").getByLabelText("New value"));
    await user.type(row("chat.self_turn_cap").getByLabelText("New value"), "7");
    await user.click(row("chat.self_turn_cap").getByRole("button", { name: "Set at this scope" }));
    await waitFor(() => expect(fake.settingsSetArgs).toHaveLength(3));
    expect(fake.settingsSetArgs[2]?.command_id).not.toBe(fake.settingsSetArgs[0]?.command_id);
  });

  it("reuses the command ID of an Unset after unreachable", async () => {
    const fake = seeded();
    fake.settingLayers["chat.self_turn_cap"] = [{ scope: SERVER, value: 6, set_by: "cli", set_at: AT }];
    const user = await open(fake);
    fake.fail("settingsUnset", "unreachable", "no answer", 1);
    const cap = row("chat.self_turn_cap");
    await user.click(cap.getByRole("button", { name: "Unset at this scope" }));
    await user.click(await cap.findByRole("button", { name: "Retry" }));
    await waitFor(() => expect(fake.settingsUnsetArgs).toHaveLength(2));
    expect(fake.settingsUnsetArgs[1]).toEqual(fake.settingsUnsetArgs[0]);
  });
});

describe("a failed read", () => {
  it("shows one error line with Retry now when settingsGet ends server, and a good retry shows the rows", async () => {
    const fake = seeded();
    fake.fail("settingsGet", "server", "the settings store is busy");
    const user = await boot(fake);
    await user.click(rail().getByRole("button", { name: "Settings" }));

    const alert = await section().findByRole("alert");
    expect(within(alert).getByText("the settings store is busy")).toBeTruthy();
    expect(section().queryByRole("group")).toBeNull();
    expect(section().getAllByRole("alert")).toHaveLength(1);

    fake.heal("settingsGet");
    await user.click(within(alert).getByRole("button", { name: "Retry now" }));
    expect(await screen.findByRole("group", { name: "Setting chat.self_turn_cap" })).toBeTruthy();
    expect(section().queryByRole("alert")).toBeNull();
  });

  it("shows the 3c banner, not an error line, when the link is down", async () => {
    const fake = seeded();
    fake.fail("settingsGet", "unauthorized", "revoked");
    fake.fail("status", "unauthorized", "revoked");
    const user = await boot(fake);
    await user.click(rail().getByRole("button", { name: "Settings" }));
    expect(await screen.findByText(/This device is no longer paired with/)).toBeTruthy();
    expect(section().queryByRole("alert")).toBeNull();
  });
});

describe("the page names no key", () => {
  it("has no setting key in Settings.tsx", () => {
    const source = readFileSync(resolve(import.meta.dirname, "../screens/Settings.tsx"), "utf8");
    expect(source).not.toMatch(/self_turn_cap|deny_commands|propose_per_min|chat\.[a-z_]+/);
  });
});
