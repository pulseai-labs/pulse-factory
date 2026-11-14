import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { App } from "../App";
import { alpha, boot, push, seeded, type User } from "./chat-helpers";
import { ADDRESS, FakeTransport, device, frames, project, runSummary } from "./fake-transport";

async function open(fake: FakeTransport) {
  const user = userEvent.setup();
  render(<App transport={fake} />);
  await screen.findByRole("navigation");
  await waitFor(() => expect(fake.count("projectList")).toBe(1));
  return user;
}

const main = () => within(screen.getByRole("main"));
const sidebar = () => within(screen.getByRole("region", { name: "Sidebar" }));

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

  it("says so when there is no project, and points at Add a project", async () => {
    const fake = new FakeTransport();
    await open(fake);
    expect(await main().findByText("No projects yet. Use Add a project to add one.")).toBeTruthy();
  });

  it("has no Add project form on the page: the picker is a dialog", async () => {
    const fake = new FakeTransport({ projects: [project(1, "alpha")] });
    await open(fake);
    expect(screen.queryByRole("form", { name: "Add project" })).toBeNull();
    expect(screen.queryByLabelText("Project root")).toBeNull();
    expect(main().getByRole("button", { name: "Add a project" })).toBeTruthy();
  });
});

// ---- Remove project (11e, `r1.s5.w5`) ----

/** alpha with one live session, beta with none, on a server named draco-desk. */
function removable(): FakeTransport {
  const fake = seeded({ projects: [project(1, "alpha"), project(2, "beta")], sessions: [alpha()] });
  fake.server = { address: ADDRESS, device: device(), server: { id: "0".repeat(64), name: "draco-desk" } };
  return fake;
}

/** Boots the page, shows the sessions, presses the remove button of `name` and waits for its confirm. */
async function openConfirm(
  fake: FakeTransport,
  name: string,
): Promise<{ user: User; confirm: ReturnType<typeof within>; runListsBefore: number }> {
  const user = await boot(fake);
  await push(fake, frames.hello(fake.sessions));
  const button = await main().findByRole("button", { name: `Remove ${name}` });
  const runListsBefore = fake.count("runList");
  await user.click(button);
  const confirm = within(await screen.findByRole("alertdialog", { name: `Remove ${name}` }));
  // The action waits for `run_list`.
  await waitFor(() => expect(fake.count("runList")).toBe(runListsBefore + 1));
  return { user, confirm, runListsBefore };
}

const enabled = (confirm: ReturnType<typeof within>, name: string) =>
  waitFor(() => expect((confirm.getByRole("button", { name }) as HTMLButtonElement).disabled).toBe(false));

describe("Remove project (11e)", () => {
  it("puts a remove button on each project block, named for the project", async () => {
    const fake = removable();
    await boot(fake);
    expect(await main().findByRole("button", { name: "Remove alpha" })).toBeTruthy();
    expect(main().getByRole("button", { name: "Remove beta" })).toBeTruthy();
    // No confirm until the button is pressed.
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });

  it("opens the confirm inside the block and reads run_list once", async () => {
    const fake = removable();
    const { confirm, runListsBefore } = await openConfirm(fake, "beta");
    // Once, and no more while the confirm stays open.
    expect(fake.count("runList")).toBe(runListsBefore + 1);
    expect(main().getByRole("group", { name: "beta" }).contains(screen.getByRole("alertdialog"))).toBe(true);
    expect(confirm.getByRole("heading", { name: "Remove beta from Pulse Factory?" })).toBeTruthy();
  });

  it("with nothing live: the folder stays, Cancel and Remove project", async () => {
    const fake = removable();
    const { user, confirm } = await openConfirm(fake, "beta");
    confirm.getByText((_: string, el: Element | null) => el?.tagName === "P" && el.textContent === "The folder /home/dev/beta stays on draco-desk.");
    expect(confirm.queryByRole("list", { name: "Live sessions" })).toBeNull();
    expect(confirm.getByRole("button", { name: "Remove project" })).toBeTruthy();
    await user.click(confirm.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(fake.count("projectRemove")).toBe(0);
  });

  it("with live sessions: lists each as agent, branch and state, and says they end", async () => {
    const fake = removable();
    const { confirm } = await openConfirm(fake, "alpha");
    const list = confirm.getByRole("list", { name: "Live sessions" });
    // `r1.s6.w4` (NG-13): the state is its own dimmed span, in lower case, after a dot.
    expect(within(list).getByText("Orchestrator · main")).toBeTruthy();
    expect(within(list).getByText("· idle")).toBeTruthy();
    confirm.getByText(
      (_: string, el: Element | null) =>
        el?.tagName === "P" &&
        el.textContent === "Removing the project ends these sessions. The folder /home/dev/alpha stays on draco-desk.",
    );
    expect(confirm.getByRole("button", { name: "End sessions and remove" })).toBeTruthy();
    expect(confirm.queryByRole("button", { name: "Remove project" })).toBeNull();
  });

  it("with an active run of the project: names the run and locks the action", async () => {
    const fake = removable();
    fake.runs = [runSummary("R-1", { state: "active", spine: "s1", project_id: "P-1" })];
    const { user, confirm } = await openConfirm(fake, "alpha");
    expect(await confirm.findByRole("heading", { name: "alpha has a workflow running" })).toBeTruthy();
    confirm.getByText(
      (_: string, el: Element | null) =>
        el?.tagName === "P" &&
        el.textContent === "Workflow s1 is running. You can remove the project after the run ends.",
    );
    const locked = confirm.getByRole("button", { name: "Remove project" }) as HTMLButtonElement;
    expect(locked.disabled).toBe(true);
    await user.click(locked);
    expect(fake.count("projectRemove")).toBe(0);
    // It does not say the sessions end.
    expect(confirm.queryByText(/ends these sessions/)).toBeNull();
  });

  it("a run of another project, an ended run and a run with no project do not lock it", async () => {
    const fake = removable();
    fake.runs = [
      runSummary("R-1", { state: "active", project_id: "P-2" }),
      runSummary("R-2", { state: "ended", project_id: "P-1" }),
      runSummary("R-3", { state: "active", project_id: null }),
    ];
    const { confirm } = await openConfirm(fake, "alpha");
    await enabled(confirm, "End sessions and remove");
    expect(confirm.queryByText(/has a workflow running/)).toBeNull();
  });

  it("removes with end_sessions and a command ID, shows the removed line, and loads the list once", async () => {
    const fake = removable();
    const { user, confirm } = await openConfirm(fake, "alpha");
    await enabled(confirm, "End sessions and remove");
    const lists = fake.count("projectList");
    await user.click(confirm.getByRole("button", { name: "End sessions and remove" }));
    expect(
      await main().findByText("alpha was removed. Its sessions ended; the folder is untouched on draco-desk."),
    ).toBeTruthy();
    expect(fake.removeArgs).toHaveLength(1);
    expect(fake.removeArgs[0]).toMatchObject({ id: "P-1", end_sessions: true });
    expect(fake.removeArgs[0]?.command_id).toMatch(/^[0-9a-f-]{16,}$/);
    expect(fake.count("projectList")).toBe(lists + 1);
    // The block and its confirm are gone; the other project stays.
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(main().queryByRole("group", { name: "alpha" })).toBeNull();
    expect(main().getByRole("group", { name: "beta" })).toBeTruthy();
  });

  it("removes a project with nothing live the same way", async () => {
    const fake = removable();
    const { user, confirm } = await openConfirm(fake, "beta");
    await enabled(confirm, "Remove project");
    await user.click(confirm.getByRole("button", { name: "Remove project" }));
    expect(
      await main().findByText("beta was removed. Its sessions ended; the folder is untouched on draco-desk."),
    ).toBeTruthy();
    expect(fake.removeArgs[0]).toMatchObject({ id: "P-2", end_sessions: true });
  });

  it("shows the server's text when it refuses, and keeps the confirm", async () => {
    const fake = removable();
    fake.fail("projectRemove", "refused", "project P-1 has an active run R-9; remove it after the run ends", 1);
    const { user, confirm } = await openConfirm(fake, "alpha");
    await enabled(confirm, "End sessions and remove");
    await user.click(confirm.getByRole("button", { name: "End sessions and remove" }));
    expect(await confirm.findByText("project P-1 has an active run R-9; remove it after the run ends")).toBeTruthy();
    expect(screen.getByRole("alertdialog")).toBeTruthy();
    expect(main().queryByText(/was removed/)).toBeNull();
  });

  it("reuses the command ID when the same remove is retried after an unreachable error", async () => {
    const fake = removable();
    fake.fail("projectRemove", "unreachable", "the server did not answer", 1);
    const { user, confirm } = await openConfirm(fake, "beta");
    await enabled(confirm, "Remove project");
    await user.click(confirm.getByRole("button", { name: "Remove project" }));
    expect(await confirm.findByText("the server did not answer")).toBeTruthy();
    // An unreachable answer takes the link down; the operator brings it back with Retry now.
    await user.click(await screen.findByRole("button", { name: "Retry now" }));
    await enabled(confirm, "Remove project");
    await user.click(confirm.getByRole("button", { name: "Remove project" }));
    expect(await main().findByText(/beta was removed/)).toBeTruthy();
    expect(fake.removeArgs).toHaveLength(2);
    expect(fake.removeArgs[1]?.command_id).toBe(fake.removeArgs[0]?.command_id);
  });
});
