// AC-10 (r1.s4.w3), rewritten by r1.s5.w4 (11k): the Settings page. The Server block, the scope chooser, and
// one block for each key the server returns: its value in force, where it comes from (the chain), a change at
// the chosen scope and "Remove this override". The page names no key: it renders the records `settings_get`
// returns (W11).
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import type { SettingScope } from "@pulse-factory/api-client";
import { screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { boot, type User } from "./chat-helpers";
import { ADDRESS, FakeTransport, orchestrator, project } from "./fake-transport";

const SERVER: SettingScope = { kind: "server", project_id: null, agent_id: null };
const ALPHA: SettingScope = { kind: "project", project_id: "P-1", agent_id: null };
const AT = "2026-11-10T10:00:00.000Z";

const rail = () => within(screen.getByRole("navigation"));
const main = () => within(screen.getByRole("main"));
const section = () => within(screen.getByRole("region", { name: "Chat policies" }));
const row = (key: string) => within(screen.getByRole("group", { name: `Setting ${key}` }));
const chain = (key: string) => row(key).getByRole("list", { name: "Comes from" });
/** The label of the chain's link that is in force. */
const live = (key: string) =>
  [...chain(key).querySelectorAll("[aria-current=true]")].map((n) => n.querySelector(".st-node-label")?.textContent);

function seeded(): FakeTransport {
  const fake = new FakeTransport({ projects: [project(1, "alpha"), project(2, "beta")] });
  fake.agents = [orchestrator(), orchestrator({ id: "autopilot", name: "Autopilot" })];
  return fake;
}

async function open(fake: FakeTransport): Promise<User> {
  const user = await boot(fake);
  await user.click(rail().getByRole("button", { name: "Settings" }));
  await screen.findByRole("region", { name: "Chat policies" });
  await screen.findByRole("group", { name: "Setting chat.self_turn_cap" });
  return user;
}

const level = (name: string) => section().getByRole("button", { name });
const pill = (group: "Project" | "Agent", name: string) => within(section().getByRole("group", { name: group })).getByRole("button", { name });

describe("the Server block", () => {
  it("shows the name, the state with the time, the address, the version, this device and its ID", async () => {
    const fake = seeded();
    await open(fake);
    const server = within(screen.getByRole("region", { name: "Server" }));
    expect(server.getByText(/^Connected · \d+ ms$/)).toBeTruthy();
    expect(server.getAllByText(ADDRESS).length).toBeGreaterThan(0);
    expect(await server.findByText("pulse-factory 0.0.0-test")).toBeTruthy();
    expect(server.getByText("MacBook")).toBeTruthy();
    expect(server.getByText("D-1")).toBeTruthy();
  });

  it("opens an in-place confirm on Disconnect this Mac, and calls forget() only on Disconnect", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.click(main().getByRole("button", { name: "Disconnect this Mac" }));
    const confirm = within(screen.getByRole("alertdialog"));
    expect(confirm.getByText(/^Disconnect from /)).toBeTruthy();
    expect(confirm.getByText("Sessions keep running. Pair again with a new code to come back.")).toBeTruthy();
    expect(fake.count("forget")).toBe(0);

    await user.click(confirm.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(fake.count("forget")).toBe(0);

    await user.click(main().getByRole("button", { name: "Disconnect this Mac" }));
    await user.click(within(screen.getByRole("alertdialog")).getByRole("button", { name: "Disconnect" }));
    await waitFor(() => expect(fake.count("forget")).toBe(1));
  });
});

describe("the Chat policies section", () => {
  it("reads the settings once when the page opens and shows one block for each key the server returns, in its order", async () => {
    const fake = seeded();
    await open(fake);
    expect(fake.settingsGetArgs).toEqual([{}]);
    expect(section().getAllByRole("group", { name: /^Setting / }).map((r) => r.getAttribute("aria-label"))).toEqual([
      "Setting chat.self_turn_cap",
      "Setting chat.deny_commands",
      "Setting chat.propose_per_min",
    ]);
  });

  it("maps the scope chooser to the four scope kinds, one read for each change, and names the scope", async () => {
    const fake = seeded();
    const user = await open(fake);
    expect(section().getByText("Show the rules for")).toBeTruthy();
    expect(section().getByText("the whole server")).toBeTruthy();

    await user.click(level("A project"));
    await waitFor(() => expect(fake.settingsGetArgs).toHaveLength(2));
    expect(fake.settingsGetArgs[1]).toEqual({ project_id: "P-1" });
    expect(section().getByText("every agent in alpha", { selector: "b" })).toBeTruthy();
    expect(level("A project").getAttribute("aria-pressed")).toBe("true");

    await user.click(pill("Project", "beta"));
    await waitFor(() => expect(fake.settingsGetArgs).toHaveLength(3));
    expect(fake.settingsGetArgs[2]).toEqual({ project_id: "P-2" });

    await user.click(level("An agent in a project"));
    await waitFor(() => expect(fake.settingsGetArgs).toHaveLength(4));
    expect(fake.settingsGetArgs[3]).toEqual({ project_id: "P-2", agent_id: "orchestrator" });
    expect(section().getByText("Orchestrator in beta", { selector: "b" })).toBeTruthy();

    // The project row keeps today's agent scope across all projects.
    await user.click(pill("Project", "All projects"));
    await waitFor(() => expect(fake.settingsGetArgs).toHaveLength(5));
    expect(fake.settingsGetArgs[4]).toEqual({ agent_id: "orchestrator" });

    await user.click(pill("Agent", "Autopilot"));
    await waitFor(() => expect(fake.settingsGetArgs).toHaveLength(6));
    expect(fake.settingsGetArgs[5]).toEqual({ agent_id: "autopilot" });

    await user.click(level("The whole server"));
    await waitFor(() => expect(fake.settingsGetArgs).toHaveLength(7));
    expect(fake.settingsGetArgs[6]).toEqual({});
  });

  it("offers a pill for each project and each agent of the server's lists", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.click(level("An agent in a project"));
    expect(within(section().getByRole("group", { name: "Project" })).getAllByRole("button").map((b) => b.textContent)).toEqual(["All projects", "alpha", "beta"]);
    await waitFor(() =>
      expect(within(section().getByRole("group", { name: "Agent" })).getAllByRole("button").map((b) => b.textContent)).toEqual(["Orchestrator", "Autopilot"]),
    );
  });

  it("shows a number as − / the value / + with its title and key, and a list as chips", async () => {
    const fake = seeded();
    fake.settingLayers["chat.deny_commands"] = [{ scope: SERVER, value: ["git push", "rm -rf /"], set_by: "cli", set_at: AT }];
    await open(fake);
    const cap = row("chat.self_turn_cap");
    expect(cap.getByText("Self turn cap")).toBeTruthy();
    expect(cap.getByText("chat.self_turn_cap")).toBeTruthy();
    expect((cap.getByLabelText("Value") as HTMLInputElement).value).toBe("4");
    expect(cap.getByRole("button", { name: "Fewer" })).toBeTruthy();
    expect(cap.getByRole("button", { name: "More" })).toBeTruthy();
    expect(cap.getByText("Applies at the next turn")).toBeTruthy();

    const deny = row("chat.deny_commands");
    expect(deny.getByText("git push")).toBeTruthy();
    expect(deny.getByRole("button", { name: "Remove rm -rf /" }).getAttribute("title")).toBe("Allow rm -rf / again");
    expect(deny.getByText("A project's or agent's own list replaces the wider list. It doesn't add to it.")).toBeTruthy();
    expect(deny.getByText("Applies at the next launch")).toBeTruthy();
  });

  it("draws the chain from Built-in to the chosen scope, with the link in force marked, and follows `from`", async () => {
    const fake = seeded();
    fake.settingLayers["chat.self_turn_cap"] = [
      { scope: SERVER, value: 6, set_by: "cli", set_at: AT },
      { scope: ALPHA, value: 2, set_by: "D-1", set_at: AT },
    ];
    const user = await open(fake);
    expect(within(chain("chat.self_turn_cap")).getAllByRole("listitem").map((n) => n.querySelector(".st-node-label")?.textContent)).toEqual(["Built-in", "Server default"]);
    expect(live("chat.self_turn_cap")).toEqual(["Server default"]);
    expect(within(chain("chat.self_turn_cap")).getByText("4")).toBeTruthy();
    expect(within(chain("chat.self_turn_cap")).getByText("6")).toBeTruthy();

    await user.click(level("A project"));
    await waitFor(() => expect(live("chat.self_turn_cap")).toEqual(["alpha"]));
    expect(within(chain("chat.self_turn_cap")).getAllByRole("listitem").map((n) => n.querySelector(".st-node-label")?.textContent)).toEqual(["Built-in", "Server default", "alpha"]);

    // A key with no layer comes from the built-in value; a link with no layer says it inherits.
    expect(live("chat.propose_per_min")).toEqual(["Built-in"]);
    expect(within(chain("chat.propose_per_min")).getAllByText("inherits")).toHaveLength(2);
  });

  it("shows no block of an earlier scope as the new scope's while the new read is out", async () => {
    const fake = seeded();
    fake.settingLayers["chat.self_turn_cap"] = [{ scope: ALPHA, value: 2, set_by: "D-1", set_at: AT }];
    const user = await open(fake);
    await user.click(level("A project"));
    await waitFor(() => expect(live("chat.self_turn_cap")).toEqual(["alpha"]));

    fake.fail("settingsGet", "server", "the settings are busy");
    await user.click(pill("Project", "beta"));
    expect(await section().findByRole("alert")).toBeTruthy();
    expect(section().queryByRole("group", { name: /^Setting / })).toBeNull();
  });
});

describe("a number", () => {
  it("sends one settingsSet for − and one for +, by one and not below 0, at the chosen scope", async () => {
    const fake = seeded();
    const user = await open(fake);
    await user.click(level("A project"));
    await waitFor(() => expect(fake.settingsGetArgs).toHaveLength(2));

    await user.click(row("chat.self_turn_cap").getByRole("button", { name: "More" }));
    await waitFor(() => expect(fake.settingsSetArgs).toHaveLength(1));
    expect(fake.settingsSetArgs[0]).toMatchObject({ key: "chat.self_turn_cap", value: 5, project_id: "P-1" });
    expect(fake.settingsSetArgs[0]?.agent_id ?? null).toBeNull();
    expect(typeof fake.settingsSetArgs[0]?.command_id).toBe("string");
    await waitFor(() => expect(live("chat.self_turn_cap")).toEqual(["alpha"]));
    expect((row("chat.self_turn_cap").getByLabelText("Value") as HTMLInputElement).value).toBe("5");
    // The answer replaced the block: no further read.
    expect(fake.settingsGetArgs).toHaveLength(2);

    await user.click(row("chat.self_turn_cap").getByRole("button", { name: "Fewer" }));
    await waitFor(() => expect(fake.settingsSetArgs).toHaveLength(2));
    expect(fake.settingsSetArgs[1]).toMatchObject({ value: 4 });
  });

  it("does not go below 0", async () => {
    const fake = seeded();
    fake.settingLayers["chat.self_turn_cap"] = [{ scope: SERVER, value: 0, set_by: "cli", set_at: AT }];
    const user = await open(fake);
    await user.click(row("chat.self_turn_cap").getByRole("button", { name: "Fewer" }));
    expect(fake.count("settingsSet")).toBe(0);
  });

  it("sends one set for a typed number on Enter and one on leaving the field, and none for a number that did not change", async () => {
    const fake = seeded();
    const user = await open(fake);
    const field = () => row("chat.self_turn_cap").getByLabelText("Value");
    await user.clear(field());
    await user.type(field(), "7{Enter}");
    await waitFor(() => expect(fake.settingsSetArgs).toHaveLength(1));
    expect(fake.settingsSetArgs[0]).toMatchObject({ key: "chat.self_turn_cap", value: 7 });
    expect(fake.settingsSetArgs[0]?.project_id ?? null).toBeNull();

    await user.clear(field());
    await user.type(field(), "9");
    await user.tab();
    await waitFor(() => expect(fake.settingsSetArgs).toHaveLength(2));
    expect(fake.settingsSetArgs[1]).toMatchObject({ value: 9 });

    // Leaving the field at the value in force sends nothing.
    await user.click(field());
    await user.tab();
    expect(fake.settingsSetArgs).toHaveLength(2);
  });

  it("does not send a field that holds no number, and says so", async () => {
    const fake = seeded();
    const user = await open(fake);
    const field = row("chat.self_turn_cap").getByLabelText("Value");
    await user.clear(field);
    await user.type(field, "{Enter}");
    expect(fake.count("settingsSet")).toBe(0);
    expect(row("chat.self_turn_cap").getByText("Enter a number.")).toBeTruthy();
  });
});

describe("a list", () => {
  it("sends one settingsSet of the whole new list for an add (Enter or the button) and for a removed chip", async () => {
    const fake = seeded();
    fake.settingLayers["chat.deny_commands"] = [{ scope: SERVER, value: ["git push", "rm -rf /"], set_by: "cli", set_at: AT }];
    const user = await open(fake);
    const deny = () => row("chat.deny_commands");

    await user.type(deny().getByLabelText("Add a pattern"), "curl | sh{Enter}");
    await waitFor(() => expect(fake.settingsSetArgs).toHaveLength(1));
    expect(fake.settingsSetArgs[0]).toMatchObject({ key: "chat.deny_commands", value: ["git push", "rm -rf /", "curl | sh"] });
    expect(fake.settingsSetArgs[0]?.project_id ?? null).toBeNull();
    await waitFor(() => expect(deny().getByText("curl | sh")).toBeTruthy());

    await user.type(deny().getByLabelText("Add a pattern"), "sudo");
    await user.click(deny().getByRole("button", { name: "Add" }));
    await waitFor(() => expect(fake.settingsSetArgs).toHaveLength(2));
    expect(fake.settingsSetArgs[1]?.value).toEqual(["git push", "rm -rf /", "curl | sh", "sudo"]);

    await user.click(deny().getByRole("button", { name: "Remove git push" }));
    await waitFor(() => expect(fake.settingsSetArgs).toHaveLength(3));
    expect(fake.settingsSetArgs[2]?.value).toEqual(["rm -rf /", "curl | sh", "sudo"]);
  });

  it("sends nothing for a blank add or a pattern the list already holds", async () => {
    const fake = seeded();
    const user = await open(fake);
    const deny = row("chat.deny_commands");
    await user.type(deny.getByLabelText("Add a pattern"), "   {Enter}");
    await user.type(deny.getByLabelText("Add a pattern"), "rm -rf{Enter}");
    expect(fake.count("settingsSet")).toBe(0);
  });

  it("refuses to remove the last chip, with a line that points at Remove this override, and sends nothing", async () => {
    const fake = seeded();
    const user = await open(fake);
    const deny = row("chat.deny_commands");
    await user.click(deny.getByRole("button", { name: "Remove rm -rf" }));
    expect(fake.count("settingsSet")).toBe(0);
    expect(deny.getByRole("alert").textContent).toBe("Use Remove this override to go back to the wider list.");
  });
});

describe("Remove this override", () => {
  it("shows only for a layer at exactly the chosen scope, and removing it shows the built-in value again", async () => {
    const fake = seeded();
    fake.settingLayers["chat.self_turn_cap"] = [{ scope: SERVER, value: 6, set_by: "cli", set_at: AT }];
    const user = await open(fake);
    expect(row("chat.self_turn_cap").getByRole("button", { name: "Remove this override" })).toBeTruthy();
    expect(row("chat.deny_commands").queryByRole("button", { name: "Remove this override" })).toBeNull();

    // The project scope has none: the server layer sits at another scope.
    await user.click(level("A project"));
    await waitFor(() => expect(live("chat.self_turn_cap")).toEqual(["Server default"]));
    expect(row("chat.self_turn_cap").queryByRole("button", { name: "Remove this override" })).toBeNull();

    await user.click(level("The whole server"));
    const remove = await waitFor(() => row("chat.self_turn_cap").getByRole("button", { name: "Remove this override" }));
    await user.click(remove);
    await waitFor(() => expect(fake.settingsUnsetArgs).toHaveLength(1));
    expect(fake.settingsUnsetArgs[0]).toMatchObject({ key: "chat.self_turn_cap" });
    expect(typeof fake.settingsUnsetArgs[0]?.command_id).toBe("string");
    await waitFor(() => expect(live("chat.self_turn_cap")).toEqual(["Built-in"]));
    expect((row("chat.self_turn_cap").getByLabelText("Value") as HTMLInputElement).value).toBe("4");
    expect(row("chat.self_turn_cap").queryByRole("button", { name: "Remove this override" })).toBeNull();
  });

  it("names no button Settings", async () => {
    await open(seeded());
    expect(main().queryAllByRole("button", { name: /settings/i })).toHaveLength(0);
    expect(rail().getAllByRole("button", { name: "Settings" })).toHaveLength(1);
  });
});

describe("a refusal and a lost answer", () => {
  it("shows the server's text on the block for a refusal, and keeps the typed value", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("settingsSet", "refused", "chat.self_turn_cap must be at least 1", 1);
    const cap = row("chat.self_turn_cap");
    await user.clear(cap.getByLabelText("Value"));
    await user.type(cap.getByLabelText("Value"), "0{Enter}");

    expect(await cap.findByText("chat.self_turn_cap must be at least 1")).toBeTruthy();
    expect(cap.queryByRole("button", { name: "Retry" })).toBeNull();
    expect((cap.getByLabelText("Value") as HTMLInputElement).value).toBe("0");
    expect(within(screen.getByRole("group", { name: "Setting chat.deny_commands" })).queryByRole("alert")).toBeNull();
  });

  it("keeps the command ID after unreachable: Retry sends the same call, and a new action gets a new ID", async () => {
    const fake = seeded();
    const user = await open(fake);
    fake.fail("settingsSet", "unreachable", "no answer", 1);
    await user.click(row("chat.self_turn_cap").getByRole("button", { name: "More" }));

    const retry = await row("chat.self_turn_cap").findByRole("button", { name: "Retry" });
    expect(fake.settingsSetArgs).toHaveLength(1);
    await user.click(retry);
    await waitFor(() => expect(fake.settingsSetArgs).toHaveLength(2));
    expect(fake.settingsSetArgs[1]).toEqual(fake.settingsSetArgs[0]);
    await waitFor(() => expect(row("chat.self_turn_cap").queryByRole("button", { name: "Retry" })).toBeNull());

    await user.click(row("chat.self_turn_cap").getByRole("button", { name: "More" }));
    await waitFor(() => expect(fake.settingsSetArgs).toHaveLength(3));
    expect(fake.settingsSetArgs[2]?.command_id).not.toBe(fake.settingsSetArgs[0]?.command_id);
  });

  it("reuses the command ID of a Remove this override after unreachable", async () => {
    const fake = seeded();
    fake.settingLayers["chat.self_turn_cap"] = [{ scope: SERVER, value: 6, set_by: "cli", set_at: AT }];
    const user = await open(fake);
    fake.fail("settingsUnset", "unreachable", "no answer", 1);
    await user.click(row("chat.self_turn_cap").getByRole("button", { name: "Remove this override" }));
    await user.click(await row("chat.self_turn_cap").findByRole("button", { name: "Retry" }));
    await waitFor(() => expect(fake.settingsUnsetArgs).toHaveLength(2));
    expect(fake.settingsUnsetArgs[1]).toEqual(fake.settingsUnsetArgs[0]);
  });
});

describe("a failed read", () => {
  it("shows one error line with Retry now when settingsGet ends server, and a good retry shows the blocks", async () => {
    const fake = seeded();
    fake.fail("settingsGet", "server", "the settings store is busy");
    const user = await boot(fake);
    await user.click(rail().getByRole("button", { name: "Settings" }));

    const alert = await section().findByRole("alert");
    expect(within(alert).getByText("the settings store is busy")).toBeTruthy();
    expect(section().queryByRole("group", { name: /^Setting / })).toBeNull();
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
