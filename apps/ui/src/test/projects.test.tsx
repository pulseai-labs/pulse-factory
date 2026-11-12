import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { App } from "../App";
import { FakeTransport, project } from "./fake-transport";

async function open(fake: FakeTransport) {
  const user = userEvent.setup();
  render(<App transport={fake} />);
  await screen.findByRole("navigation");
  await waitFor(() => expect(fake.count("projectList")).toBe(1));
  return user;
}

const main = () => within(screen.getByRole("main"));
const sidebar = () => within(screen.getByRole("region", { name: "Sidebar" }));

async function submitAdd(user: ReturnType<typeof userEvent.setup>, root: string, name?: string) {
  const rootInput = screen.getByLabelText("Project root");
  await user.clear(rootInput);
  await user.type(rootInput, root);
  const nameInput = screen.getByLabelText("Name (optional)");
  await user.clear(nameInput);
  if (name) await user.type(nameInput, name);
  await user.click(screen.getByRole("button", { name: "Add project" }));
}

// An `unreachable` add takes the link down (11i), and the add form is disabled while it is down (Approach 7).
// The operator brings it back with "Retry now" in the banner, then submits again.
async function bringLinkBack(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole("button", { name: "Retry now" }));
  await waitFor(() => expect((screen.getByLabelText("Project root") as HTMLInputElement).disabled).toBe(false));
}

describe("the Projects page (2c)", () => {
  it("shows the live projects in the list and in the sidebar", async () => {
    const fake = new FakeTransport({ projects: [project(1, "alpha"), project(2, "beta")] });
    await open(fake);
    expect(await main().findByText("alpha")).toBeTruthy();
    expect(main().getByText("/home/dev/beta")).toBeTruthy();
    expect(sidebar().getByText("alpha")).toBeTruthy();
    expect(sidebar().getByText("beta")).toBeTruthy();
    // 11e: no subtitle while the link is up. One block per project, each with its New session button.
    expect(main().queryByText(/projects on /)).toBeNull();
    expect(main().getByRole("button", { name: "New session in alpha" })).toBeTruthy();
    expect(main().getByRole("button", { name: "New session in beta" })).toBeTruthy();
  });

  it("says so when there is no project", async () => {
    const fake = new FakeTransport();
    await open(fake);
    expect(await main().findByText("No projects yet. Add one below.")).toBeTruthy();
  });

  it("adds a project with a command ID, then fetches the list once", async () => {
    const fake = new FakeTransport();
    const user = await open(fake);
    await submitAdd(user, "/home/dev/shiny", "Shiny");

    expect(await main().findByText("Shiny")).toBeTruthy();
    expect(sidebar().getByText("Shiny")).toBeTruthy();
    expect(fake.addArgs).toHaveLength(1);
    expect(fake.addArgs[0]).toMatchObject({ root: "/home/dev/shiny", name: "Shiny" });
    expect(fake.addArgs[0]?.command_id).toMatch(/^[0-9a-f-]{16,}$/);
    // The shell's sidebar reads the run list once when it mounts (Approach 7).
    expect(fake.calls).toEqual(["paired", "runList", "projectList", "openStream", "projectAdd", "projectList"]);
    // The form is empty again.
    expect((screen.getByLabelText("Project root") as HTMLInputElement).value).toBe("");
  });

  it("sends no name when the name is empty", async () => {
    const fake = new FakeTransport();
    const user = await open(fake);
    await submitAdd(user, "/home/dev/plain");
    await main().findByText("plain");
    expect(fake.addArgs[0]).not.toHaveProperty("name");
  });

  it("reuses the command ID when the same submit is retried after an unreachable error", async () => {
    const fake = new FakeTransport();
    fake.fail("projectAdd", "unreachable", "the server did not answer", 1);
    const user = await open(fake);

    await submitAdd(user, "/home/dev/again", "Again");
    expect(await screen.findByText("the server did not answer")).toBeTruthy();
    expect(fake.addArgs).toHaveLength(1);
    expect((screen.getByLabelText("Project root") as HTMLInputElement).value).toBe("/home/dev/again");

    await bringLinkBack(user);
    await user.click(screen.getByRole("button", { name: "Add project" }));
    expect(await main().findByText("Again")).toBeTruthy();
    expect(fake.addArgs).toHaveLength(2);
    expect(fake.addArgs[1]?.command_id).toBe(fake.addArgs[0]?.command_id);

    // A new submit after a success is a new command.
    await submitAdd(user, "/home/dev/other");
    await main().findByText("other");
    expect(fake.addArgs[2]?.command_id).not.toBe(fake.addArgs[0]?.command_id);
  });

  it("uses a new command ID when the content changes after an unreachable error", async () => {
    const fake = new FakeTransport();
    fake.fail("projectAdd", "unreachable", "the server did not answer", 1);
    const user = await open(fake);
    await submitAdd(user, "/home/dev/one");
    await screen.findByText("the server did not answer");
    await bringLinkBack(user);
    await submitAdd(user, "/home/dev/two");
    await main().findByText("two");
    expect(fake.addArgs[1]?.command_id).not.toBe(fake.addArgs[0]?.command_id);
  });

  it("shows the server's text under the form for a refused add, and keeps the form", async () => {
    const fake = new FakeTransport();
    fake.fail("projectAdd", "refused", "the root does not exist: /nope", 1);
    const user = await open(fake);
    await submitAdd(user, "/nope");

    const form = within(screen.getByRole("form", { name: "Add project" }));
    expect(await form.findByText("the root does not exist: /nope")).toBeTruthy();
    expect((screen.getByLabelText("Project root") as HTMLInputElement).value).toBe("/nope");
    // A refusal is no disconnect: no banner.
    expect(screen.queryByText(/Not connected/)).toBeNull();
  });

  it("refuses an empty root before any call", async () => {
    const fake = new FakeTransport();
    const user = await open(fake);
    await user.click(screen.getByRole("button", { name: "Add project" }));
    expect(await screen.findByText(/Enter the absolute path/)).toBeTruthy();
    expect(fake.count("projectAdd")).toBe(0);
  });
});
