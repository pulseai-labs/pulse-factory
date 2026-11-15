// `r1.s6.w4`: the gaps of the second walk that the Add a project dialog (11f) and the Remove project confirm
// (11e) close: the host row's server icon (NG-17), the breadcrumb (NG-14), the taken folder's row (NG-15),
// the "+" of Add project (NG-16), the live-session confirm's state words (NG-13) and the locked confirm
// (KL-8, ruling P9). The call tests of both screens stay in add-project.test.tsx and projects.test.tsx.
import type { ChatSessionRecord } from "@pulse-factory/api-client";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { App } from "../App";
import { alpha, boot, push, seeded as seededChat, type User } from "./chat-helpers";
import {
  ADDRESS,
  FakeTransport,
  chatSession,
  device,
  folderEntry,
  folderList,
  frames,
  project,
  runSummary,
} from "./fake-transport";

const SERVER = { address: ADDRESS, device: device(), server: { id: "0".repeat(64), name: "draco-desk" } };
const main = () => within(screen.getByRole("main"));

// ---- Add a project (11f) ----

/** One project, and a home folder with a taken folder, a plain folder and a folder with folders below it. */
function folders(): FakeTransport {
  const fake = new FakeTransport({ projects: [project(1, "alpha")], server: SERVER });
  fake.folders["/home/dev"] = folderList([
    folderEntry("alpha", { git: true, branch: "main", project_id: "P-1" }),
    folderEntry("notes"),
  ]);
  fake.folders["/home/dev/notes"] = folderList([folderEntry("deep", {}, "/home/dev/notes")], { path: "/home/dev/notes" });
  fake.folders["/home/dev/notes/deep"] = folderList([folderEntry("leaf", {}, "/home/dev/notes/deep")], {
    path: "/home/dev/notes/deep",
  });
  return fake;
}

async function openDialog(fake: FakeTransport): Promise<{ user: User; dialog: ReturnType<typeof within> }> {
  const user = userEvent.setup();
  render(<App transport={fake} />);
  await screen.findByRole("navigation");
  await waitFor(() => expect(fake.count("projectList")).toBe(1));
  await user.click(screen.getByRole("button", { name: "Add a project" }));
  const el = await screen.findByRole("dialog", { name: "Add a project" });
  await within(el).findByText("notes");
  return { user, dialog: within(el) };
}

const path = (dialog: ReturnType<typeof within>) => dialog.getByRole("group", { name: "Folder path" });

describe("Add a project: the host row and the button (NG-17, NG-16)", () => {
  it("draws the host's icon as the server icon, not the layers icon", async () => {
    const { dialog } = await openDialog(folders());
    const host = dialog.getByRole("region", { name: "Host" });
    expect(host.querySelector('[data-icon="server"]')).not.toBeNull();
    expect(host.querySelector('[data-icon="layers"]')).toBeNull();
    expect(within(host).getByText("draco-desk")).toBeTruthy();
  });

  it("gives Add project the plus icon", async () => {
    const { dialog } = await openDialog(folders());
    const add = dialog.getByRole("button", { name: "Add project" });
    expect(add.querySelector('[data-icon="plus"]')).not.toBeNull();
  });
});

describe("Add a project: the breadcrumb (NG-14)", () => {
  it("at the home folder shows the home part alone, with the full path in its title", async () => {
    const { dialog } = await openDialog(folders());
    const crumbs = path(dialog);
    const home = within(crumbs).getByTitle("/home/dev");
    expect(home.textContent).toBe("~");
    expect(home.querySelector('[data-icon="house"]')).not.toBeNull();
    expect(home.getAttribute("aria-current")).toBe("location");
    // Nothing else: the up button, and no other part, no separator.
    expect(within(crumbs).getAllByRole("button").map((b) => b.getAttribute("title"))).toEqual(["Up one folder"]);
    expect(crumbs.textContent).not.toContain("/");
  });

  it("below home shows the home button, one button per folder and the current folder as the location", async () => {
    const fake = folders();
    const { user, dialog } = await openDialog(fake);
    await user.click(dialog.getByRole("button", { name: "Open notes" }));
    await dialog.findByText("deep");
    await user.click(dialog.getByRole("button", { name: "Open deep" }));
    await dialog.findByText("leaf");
    const crumbs = path(dialog);
    const buttons = within(crumbs).getAllByRole("button");
    expect(buttons.map((b) => b.getAttribute("title") ?? b.textContent)).toEqual(["Up one folder", "/home/dev", "notes"]);
    expect(buttons[1]?.textContent).toBe("~");
    expect(buttons[1]?.querySelector('[data-icon="house"]')).not.toBeNull();
    const now = crumbs.querySelector('[aria-current="location"]');
    expect(now?.textContent).toBe("deep");
    expect(now?.tagName).not.toBe("BUTTON");
    expect(crumbs.querySelectorAll('[aria-current="location"]').length).toBe(1);
    // Two separators: "~ / notes / deep".
    expect(crumbs.textContent?.match(/\//g)?.length).toBe(2);
  });

  it("makes one folder_list call for the path of the crumb that is pressed", async () => {
    const fake = folders();
    const { user, dialog } = await openDialog(fake);
    await user.click(dialog.getByRole("button", { name: "Open notes" }));
    await dialog.findByText("deep");
    await user.click(dialog.getByRole("button", { name: "Open deep" }));
    await dialog.findByText("leaf");
    const before = fake.count("folderList");
    await user.click(within(path(dialog)).getByRole("button", { name: "notes" }));
    await dialog.findByText("deep");
    expect(fake.count("folderList")).toBe(before + 1);
    expect(fake.folderArgs.at(-1)).toEqual({ path: "/home/dev/notes" });
    await user.click(within(path(dialog)).getByTitle("/home/dev"));
    await dialog.findByText("alpha");
    expect(fake.count("folderList")).toBe(before + 2);
    expect(fake.folderArgs.at(-1)).toEqual({ path: "/home/dev" });
  });
});

describe("Add a project: a folder that is a project already (NG-15)", () => {
  it("has no Open button, a disabled pick and the label; a plain folder keeps its Open button", async () => {
    const { dialog } = await openDialog(folders());
    expect(dialog.queryByRole("button", { name: "Open alpha" })).toBeNull();
    const row = dialog.getByRole("button", { name: /^alpha/ }) as HTMLButtonElement;
    expect(row.disabled).toBe(true);
    expect(within(row).getByText("Already a project")).toBeTruthy();
    expect(dialog.getByRole("button", { name: "Open notes" })).toBeTruthy();
    const plain = dialog.getByRole("button", { name: /^notes/ }) as HTMLButtonElement;
    expect(plain.disabled).toBe(false);
    expect(within(plain).queryByText("Already a project")).toBeNull();
  });
});

// ---- Remove project (11e) ----

/** alpha with the sessions `sessions`, beta with none, on a server named draco-desk. */
function removable(sessions: ChatSessionRecord[] = [alpha()]): FakeTransport {
  const fake = seededChat({ projects: [project(1, "alpha"), project(2, "beta")], sessions });
  fake.server = SERVER;
  return fake;
}

async function openConfirm(fake: FakeTransport): Promise<{ user: User; confirm: ReturnType<typeof within> }> {
  const user = await boot(fake);
  await push(fake, frames.hello(fake.sessions));
  const before = fake.count("runList");
  await user.click(await main().findByRole("button", { name: "Remove alpha" }));
  const confirm = within(await screen.findByRole("alertdialog", { name: "Remove alpha" }));
  await waitFor(() => expect(fake.count("runList")).toBe(before + 1));
  return { user, confirm };
}

describe("Remove project: the live-session confirm (NG-13)", () => {
  it("reads each session as agent and branch, then its state dimmed, in lower case, after a dot", async () => {
    const fake = removable([
      chatSession("S-1", { project_id: "P-1", project_name: "alpha", state: "needs_you" }),
      chatSession("S-2", { project_id: "P-1", project_name: "alpha", state: "idle" }),
      chatSession("S-3", { project_id: "P-1", project_name: "alpha", state: "working" }),
    ]);
    const { confirm } = await openConfirm(fake);
    const items = within(confirm.getByRole("list", { name: "Live sessions" })).getAllByRole("listitem");
    expect(items.length).toBe(3);
    const read = items.map((li) => Array.from(li.querySelectorAll("span:not(.ui-dot)")).map((s) => s.textContent));
    expect(read).toEqual([
      ["Orchestrator · main", "· waiting on you"],
      ["Orchestrator · main", "· idle"],
      ["Orchestrator · main", "· working"],
    ]);
    const list = confirm.getByRole("list", { name: "Live sessions" });
    expect(list.textContent).not.toMatch(/Idle|Working|Needs you/);
    // The state is the dimmed span.
    expect(items[0]?.querySelector(".rm-state")?.textContent).toBe("· waiting on you");
  });
});

describe("Remove project: the locked confirm (KL-8, P9)", () => {
  it("one run: 11e's sentence with the spine's ID, and the locked button's title", async () => {
    const fake = removable();
    fake.runs = [runSummary("R-1", { state: "active", spine: "r0.s19", project_id: "P-1" })];
    const { user, confirm } = await openConfirm(fake);
    expect(await confirm.findByRole("heading", { name: "alpha has a workflow running" })).toBeTruthy();
    confirm.getByText(
      (_: string, el: Element | null) =>
        el?.tagName === "P" && el.textContent === "Workflow r0.s19 is running. You can remove the project after the run ends.",
    );
    const locked = confirm.getByRole("button", { name: "Remove project" }) as HTMLButtonElement;
    expect(locked.title).toBe("Locked while r0.s19 runs");
    expect(locked.disabled).toBe(true);
    await user.click(locked);
    expect(fake.count("projectRemove")).toBe(0);
  });

  it("two runs: the plural sentence, and the locked button's title for its workflows", async () => {
    const fake = removable();
    fake.runs = [
      runSummary("R-1", { state: "active", spine: "r0.s19", project_id: "P-1" }),
      runSummary("R-2", { state: "active", spine: "r1.s6", project_id: "P-1" }),
    ];
    const { user, confirm } = await openConfirm(fake);
    await confirm.findByRole("heading", { name: "alpha has a workflow running" });
    confirm.getByText(
      (_: string, el: Element | null) =>
        el?.tagName === "P" &&
        el.textContent === "Workflows r0.s19, r1.s6 are running. You can remove the project after the run ends.",
    );
    const locked = confirm.getByRole("button", { name: "Remove project" }) as HTMLButtonElement;
    expect(locked.title).toBe("Locked while its workflows run");
    await user.click(locked);
    expect(fake.count("projectRemove")).toBe(0);
  });
});
