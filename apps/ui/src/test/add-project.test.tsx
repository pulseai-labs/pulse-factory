// `r1.s5.w5`: the Add a project dialog (11f). It lists the server's home folder with `folder_list`, marks
// the git folders and the folders that are projects already, names the project after the folder and
// offers `git init`. Each case is its own test. The remove confirm (11e) is tested in projects.test.tsx.
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { App } from "../App";
import { FakeTransport, device, folderEntry, folderList, project, ADDRESS } from "./fake-transport";

type User = ReturnType<typeof userEvent.setup>;

/** A paragraph whose whole text matches `re`: a sentence with an inline path or code is several text nodes. */
const para = (scope: ReturnType<typeof within>, re: RegExp) =>
  scope.getByText((_: string, el: Element | null) => el?.tagName === "P" && re.test(el.textContent ?? ""));

/** A fake with one project and a home folder of five folders, on a server named draco-desk. */
function seeded(): FakeTransport {
  const fake = new FakeTransport({
    projects: [project(1, "alpha")],
    server: { address: ADDRESS, device: device(), server: { id: "0".repeat(64), name: "draco-desk" } },
  });
  fake.folders["/home/dev"] = folderList([
    folderEntry("alpha", { git: true, branch: "main", project_id: "P-1" }),
    folderEntry("notes"),
    folderEntry("shiny", { git: true, branch: "main" }),
    folderEntry("scratch"),
    folderEntry("wip", { git: true, branch: null }),
  ]);
  fake.folders["/home/dev/notes"] = folderList([folderEntry("deep", {}, "/home/dev/notes")], { path: "/home/dev/notes" });
  fake.folders["/home/dev/deep-repo"] = folderList([folderEntry("src", {}, "/home/dev/deep-repo")], {
    path: "/home/dev/deep-repo",
    repo: "/home/dev/deep-repo",
  });
  return fake;
}

async function open(fake: FakeTransport): Promise<User> {
  const user = userEvent.setup();
  render(<App transport={fake} />);
  await screen.findByRole("navigation");
  await waitFor(() => expect(fake.count("projectList")).toBe(1));
  return user;
}

/** Opens the dialog and waits for the row named `row` of the home folder's listing. */
async function openDialog(fake: FakeTransport, row = "scratch"): Promise<{ user: User; dialog: ReturnType<typeof within> }> {
  const user = await open(fake);
  await user.click(screen.getByRole("button", { name: "Add a project" }));
  const el = await screen.findByRole("dialog", { name: "Add a project" });
  await within(el).findByText(row);
  return { user, dialog: within(el) };
}

const main = () => within(screen.getByRole("main"));
const sidebar = () => within(screen.getByRole("region", { name: "Sidebar" }));

async function pick(user: User, dialog: ReturnType<typeof within>, name: string) {
  await user.click(dialog.getByRole("button", { name: new RegExp(`^${name}( |$)`) }));
}

describe("the Add a project button and the dialog (11f)", () => {
  it("puts Add a project in the page header and removes today's form", async () => {
    const fake = seeded();
    await open(fake);
    const header = screen.getByRole("button", { name: "Add a project" });
    expect(header).toBeTruthy();
    expect(screen.queryByLabelText("Project root")).toBeNull();
    expect(screen.queryByRole("form", { name: "Add project" })).toBeNull();
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("says where to add the first project when there is none", async () => {
    const fake = new FakeTransport();
    await open(fake);
    expect(await main().findByText("No projects yet. Use Add a project to add one.")).toBeTruthy();
  });

  it("opens a modal dialog named Add a project with its sub-line", async () => {
    const fake = seeded();
    const { dialog } = await openDialog(fake);
    const el = screen.getByRole("dialog", { name: "Add a project" });
    expect(el.getAttribute("aria-modal")).toBe("true");
    expect(el.classList.contains("add-project")).toBe(true);
    expect(dialog.getByText("Add a project", { selector: "h2" })).toBeTruthy();
    expect(dialog.getByText("A project is a folder on your server. Its sessions and runs start there.")).toBeTruthy();
  });

  it("shows the server as the host and Browse folder as the way", async () => {
    const fake = seeded();
    const { dialog } = await openDialog(fake);
    expect(dialog.getByText("Connected")).toBeTruthy();
    expect(dialog.getByText("Browse folder")).toBeTruthy();
    expect(dialog.getByText("Pick a folder that is already on draco-desk.")).toBeTruthy();
    expect(dialog.getByText("Only folders inside your home folder")).toBeTruthy();
  });

  it("shows Other hosts, Clone from URL and Create new project as not built, with Later release", async () => {
    const fake = seeded();
    const { dialog } = await openDialog(fake);
    for (const label of ["Other hosts", "Clone from URL", "Create new project"]) {
      const node = dialog.getByText(label).closest("[data-not-built]");
      expect(node, label).not.toBeNull();
      expect(node?.getAttribute("aria-disabled")).toBe("true");
      expect(within(node as HTMLElement).getByText("Later release")).toBeTruthy();
    }
    expect(dialog.getByText("Clone a repository onto draco-desk.")).toBeTruthy();
    expect(dialog.getByText("Start an empty repository.")).toBeTruthy();
  });

  it("moves focus into the dialog and gives it back to the header button on Cancel", async () => {
    const fake = seeded();
    const { user, dialog } = await openDialog(fake);
    const el = screen.getByRole("dialog");
    expect(el.contains(document.activeElement)).toBe(true);
    await user.click(dialog.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add a project" }));
  });

  it("closes on Escape and gives focus back to the header button", async () => {
    const fake = seeded();
    const { user } = await openDialog(fake);
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add a project" }));
  });

  it("closes on the close button", async () => {
    const fake = seeded();
    const { user, dialog } = await openDialog(fake);
    await user.click(dialog.getByRole("button", { name: "Close" }));
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});

describe("the folder list (11f)", () => {
  it("calls folder_list once when it opens, with no path, and never again by itself", async () => {
    const fake = seeded();
    const { user } = await openDialog(fake);
    expect(fake.count("folderList")).toBe(1);
    expect(fake.folderArgs[0]).toBeUndefined();
    // Picking a row and typing are no navigation.
    await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: /^scratch/ }));
    await user.type(within(screen.getByRole("dialog")).getByLabelText("Project name"), "x");
    expect(fake.count("folderList")).toBe(1);
  });

  it("lists the folders with the git mark and the branch", async () => {
    const fake = seeded();
    const { dialog } = await openDialog(fake);
    const shiny = dialog.getByRole("button", { name: /^shiny/ });
    expect(within(shiny).getByText("git · main")).toBeTruthy();
    // A detached HEAD has the git mark and no branch.
    expect(within(dialog.getByRole("button", { name: /^wip/ })).getByText("git")).toBeTruthy();
    // A plain folder has no mark.
    expect(within(dialog.getByRole("button", { name: /^notes/ })).queryByText(/git/)).toBeNull();
  });

  it("marks a folder that is a project already, and it cannot be picked", async () => {
    const fake = seeded();
    const { user, dialog } = await openDialog(fake);
    const row = dialog.getByRole("button", { name: /^alpha/ }) as HTMLButtonElement;
    expect(within(row).getByText("Already a project")).toBeTruthy();
    expect(row.disabled).toBe(true);
    await user.click(row);
    expect((dialog.getByLabelText("Project name") as HTMLInputElement).value).toBe("");
    expect(dialog.getByRole("button", { name: "Add project" }).hasAttribute("disabled")).toBe(true);
  });

  it("names the project after the picked folder and lets the operator change the name", async () => {
    const fake = seeded();
    const { user, dialog } = await openDialog(fake);
    const name = dialog.getByLabelText("Project name") as HTMLInputElement;
    expect(name.value).toBe("");
    await pick(user, dialog, "scratch");
    expect(name.value).toBe("scratch");
    // The full path shows under the field.
    expect(dialog.getByText("/home/dev/scratch")).toBeTruthy();
    await user.clear(name);
    await user.type(name, "Sketches");
    expect(name.value).toBe("Sketches");
    // Another pick renames it.
    await pick(user, dialog, "notes");
    expect(name.value).toBe("notes");
  });

  it("opens a folder with its open button and lists it, once per navigation", async () => {
    const fake = seeded();
    const { user, dialog } = await openDialog(fake);
    await user.click(dialog.getByRole("button", { name: "Open notes" }));
    expect(await dialog.findByText("deep")).toBeTruthy();
    expect(fake.count("folderList")).toBe(2);
    expect(fake.folderArgs[1]).toEqual({ path: "/home/dev/notes" });
    // The breadcrumb goes back to the home folder: one more call.
    await user.click(dialog.getByRole("button", { name: "/home/dev" }));
    expect(await dialog.findByText("scratch")).toBeTruthy();
    expect(fake.count("folderList")).toBe(3);
    expect(fake.folderArgs[2]).toEqual({ path: "/home/dev" });
  });

  it("goes up with the up button, and the up button is off at the home folder", async () => {
    const fake = seeded();
    const { user, dialog } = await openDialog(fake);
    expect((dialog.getByRole("button", { name: "Up one folder" }) as HTMLButtonElement).disabled).toBe(true);
    await user.click(dialog.getByRole("button", { name: "Open notes" }));
    await dialog.findByText("deep");
    const up = dialog.getByRole("button", { name: "Up one folder" }) as HTMLButtonElement;
    expect(up.disabled).toBe(false);
    await user.click(up);
    await dialog.findByText("scratch");
    expect(fake.folderArgs.at(-1)).toEqual({ path: "/home/dev" });
  });

  it("clears the pick when the operator opens another folder", async () => {
    const fake = seeded();
    const { user, dialog } = await openDialog(fake);
    await pick(user, dialog, "scratch");
    await user.click(dialog.getByRole("button", { name: "Open notes" }));
    await dialog.findByText("deep");
    expect((dialog.getByLabelText("Project name") as HTMLInputElement).value).toBe("");
  });

  it("shows the server's text when the listing is refused, and keeps the dialog", async () => {
    const fake = seeded();
    fake.fail("folderList", "refused", "the server has no home folder", 1);
    const user = await open(fake);
    await user.click(screen.getByRole("button", { name: "Add a project" }));
    const dialog = within(await screen.findByRole("dialog", { name: "Add a project" }));
    expect(await dialog.findByText("the server has no home folder")).toBeTruthy();
    expect(dialog.queryByText("scratch")).toBeNull();
  });

  it("says when the list stops at 500", async () => {
    const fake = seeded();
    fake.folders["/home/dev"] = folderList([folderEntry("a")], { truncated: true });
    const { dialog } = await openDialog(fake, "a");
    expect(dialog.getByText("Only the first 500 folders are shown.")).toBeTruthy();
  });
});

describe("the git init tip (11f)", () => {
  it("shows for a picked folder that is not a repository, with the box off", async () => {
    const fake = seeded();
    const { user, dialog } = await openDialog(fake);
    expect(dialog.queryByText("Tip: make it a git repository")).toBeNull();
    await pick(user, dialog, "scratch");
    expect(dialog.getByText("Tip: make it a git repository")).toBeTruthy();
    expect(dialog.getByText("Pulse Factory can set it up when you add it.")).toBeTruthy();
    const box = dialog.getByRole("checkbox", { name: /Run git init in this folder when I add it/ }) as HTMLInputElement;
    expect(box.checked).toBe(false);
    para(dialog, /^Or run git init on draco-desk yourself\.$/);
  });

  it("does not show for a folder that is a repository", async () => {
    const fake = seeded();
    const { user, dialog } = await openDialog(fake);
    await pick(user, dialog, "shiny");
    expect(dialog.queryByText("Tip: make it a git repository")).toBeNull();
    expect(dialog.queryByRole("checkbox")).toBeNull();
  });

  it("does not show inside a repository, and says the folder is added as a folder", async () => {
    const fake = seeded();
    fake.folders["/home/dev"] = folderList([folderEntry("deep-repo", { git: true, branch: "main" })]);
    const { user, dialog } = await openDialog(fake, "deep-repo");
    await user.click(dialog.getByRole("button", { name: "Open deep-repo" }));
    await dialog.findByText("src");
    await pick(user, dialog, "src");
    expect(dialog.queryByText("Tip: make it a git repository")).toBeNull();
    expect(dialog.queryByRole("checkbox")).toBeNull();
    expect(dialog.getByText(/inside the git repository/i)).toBeTruthy();
    expect(dialog.getByText(/added as a folder/i)).toBeTruthy();
  });

  it("drops the tick when another folder is picked", async () => {
    const fake = seeded();
    const { user, dialog } = await openDialog(fake);
    await pick(user, dialog, "scratch");
    await user.click(dialog.getByRole("checkbox"));
    await pick(user, dialog, "notes");
    expect((dialog.getByRole("checkbox") as HTMLInputElement).checked).toBe(false);
  });
});

describe("adding the project (11f)", () => {
  it("is off until a folder is picked and the name is not empty", async () => {
    const fake = seeded();
    const { user, dialog } = await openDialog(fake);
    const add = () => dialog.getByRole("button", { name: "Add project" }) as HTMLButtonElement;
    expect(add().disabled).toBe(true);
    await pick(user, dialog, "scratch");
    expect(add().disabled).toBe(false);
    await user.clear(dialog.getByLabelText("Project name"));
    expect(add().disabled).toBe(true);
  });

  it("sends the folder, the name and a command ID, and no git_init when the box is off", async () => {
    const fake = seeded();
    const { user, dialog } = await openDialog(fake);
    await pick(user, dialog, "scratch");
    await user.click(dialog.getByRole("button", { name: "Add project" }));
    expect(await dialog.findByText("scratch is a project")).toBeTruthy();
    expect(fake.addArgs).toHaveLength(1);
    expect(fake.addArgs[0]).toMatchObject({ root: "/home/dev/scratch", name: "scratch" });
    expect(fake.addArgs[0]).not.toHaveProperty("git_init");
    expect(fake.addArgs[0]?.command_id).toMatch(/^[0-9a-f-]{16,}$/);
  });

  it("sends the name the operator typed", async () => {
    const fake = seeded();
    const { user, dialog } = await openDialog(fake);
    await pick(user, dialog, "scratch");
    const name = dialog.getByLabelText("Project name");
    await user.clear(name);
    await user.type(name, "Sketches");
    await user.click(dialog.getByRole("button", { name: "Add project" }));
    await dialog.findByText("Sketches is a project");
    expect(fake.addArgs[0]).toMatchObject({ root: "/home/dev/scratch", name: "Sketches" });
  });

  it("sends git_init: true only when the box is ticked", async () => {
    const fake = seeded();
    const { user, dialog } = await openDialog(fake);
    await pick(user, dialog, "scratch");
    await user.click(dialog.getByRole("checkbox", { name: /Run git init in this folder when I add it/ }));
    await user.click(dialog.getByRole("button", { name: "Add project" }));
    await dialog.findByText("scratch is a project");
    expect(fake.addArgs[0]).toMatchObject({ root: "/home/dev/scratch", name: "scratch", git_init: true });
  });

  it("shows the success view: where sessions start, and no git line without git init", async () => {
    const fake = seeded();
    const { user, dialog } = await openDialog(fake);
    await pick(user, dialog, "scratch");
    await user.click(dialog.getByRole("button", { name: "Add project" }));
    expect(await dialog.findByText("scratch is a project")).toBeTruthy();
    para(dialog, /Sessions start in \/home\/dev\/scratch on draco-desk\./);
    expect(dialog.queryByText(/Ran git init/)).toBeNull();
    expect(dialog.getByRole("button", { name: "Add another" })).toBeTruthy();
    expect(dialog.getByRole("button", { name: "Open project" })).toBeTruthy();
  });

  it("shows the git line with the branch when git init ran, read once from the parent folder", async () => {
    const fake = seeded();
    const { user, dialog } = await openDialog(fake);
    await pick(user, dialog, "scratch");
    await user.click(dialog.getByRole("checkbox"));
    // After the init the folder is a repository on `main`: the next listing says so.
    const before = fake.count("folderList");
    fake.folders["/home/dev"] = folderList([
      folderEntry("scratch", { git: true, branch: "main", project_id: "P-2" }),
    ]);
    await user.click(dialog.getByRole("button", { name: "Add project" }));
    expect(
      await dialog.findByText("Ran git init in the folder. It is now a git repository on branch main."),
    ).toBeTruthy();
    expect(fake.count("folderList")).toBe(before + 1);
    expect(fake.folderArgs.at(-1)).toEqual({ path: "/home/dev" });
  });

  it("reloads the project list once, and the project shows in the page and the sidebar", async () => {
    const fake = seeded();
    const { user, dialog } = await openDialog(fake);
    await pick(user, dialog, "scratch");
    await user.click(dialog.getByRole("button", { name: "Add project" }));
    await dialog.findByText("scratch is a project");
    expect(fake.count("projectAdd")).toBe(1);
    expect(fake.count("projectList")).toBe(2);
    await user.click(dialog.getByRole("button", { name: "Open project" }));
    expect(await main().findByText("scratch")).toBeTruthy();
    expect(sidebar().getByText("scratch")).toBeTruthy();
  });

  it("Add another goes back to the picker, with a fresh listing and nothing picked", async () => {
    const fake = seeded();
    const { user, dialog } = await openDialog(fake);
    await pick(user, dialog, "scratch");
    await user.click(dialog.getByRole("button", { name: "Add project" }));
    await dialog.findByText("scratch is a project");
    fake.folders["/home/dev"] = folderList([folderEntry("scratch", { project_id: "P-2" }), folderEntry("notes")]);
    await user.click(dialog.getByRole("button", { name: "Add another" }));
    expect(await dialog.findByText("Already a project")).toBeTruthy();
    expect((dialog.getByLabelText("Project name") as HTMLInputElement).value).toBe("");
    expect(dialog.queryByText("scratch is a project")).toBeNull();
    expect(screen.getByRole("dialog")).toBeTruthy();
  });

  it("Open project closes the dialog and focuses the new project's block", async () => {
    const fake = seeded();
    const { user, dialog } = await openDialog(fake);
    await pick(user, dialog, "scratch");
    await user.click(dialog.getByRole("button", { name: "Add project" }));
    await dialog.findByText("scratch is a project");
    await user.click(dialog.getByRole("button", { name: "Open project" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    const block = await main().findByRole("group", { name: "scratch" });
    expect(document.activeElement).toBe(block);
  });

  it("shows the server's text for a refused add, and keeps the picker", async () => {
    const fake = seeded();
    fake.fail("projectAdd", "refused", "the root is not under the home folder: /home/dev/scratch", 1);
    const { user, dialog } = await openDialog(fake);
    await pick(user, dialog, "scratch");
    await user.click(dialog.getByRole("button", { name: "Add project" }));
    expect(await dialog.findByText("the root is not under the home folder: /home/dev/scratch")).toBeTruthy();
    expect((dialog.getByLabelText("Project name") as HTMLInputElement).value).toBe("scratch");
    // A refusal is no disconnect: no banner.
    expect(screen.queryByText(/Not connected/)).toBeNull();
  });

  it("reuses the command ID when the same submit is retried after an unreachable error", async () => {
    const fake = seeded();
    fake.fail("projectAdd", "unreachable", "the server did not answer", 1);
    const { user, dialog } = await openDialog(fake);
    await pick(user, dialog, "scratch");
    await user.click(dialog.getByRole("button", { name: "Add project" }));
    expect(await dialog.findByText("the server did not answer")).toBeTruthy();
    expect(fake.addArgs).toHaveLength(1);
    // An unreachable answer takes the link down; the operator brings it back with Retry now.
    await user.click(await screen.findByRole("button", { name: "Retry now" }));
    await waitFor(() => expect((dialog.getByRole("button", { name: "Add project" }) as HTMLButtonElement).disabled).toBe(false));
    await user.click(dialog.getByRole("button", { name: "Add project" }));
    expect(await dialog.findByText("scratch is a project")).toBeTruthy();
    expect(fake.addArgs).toHaveLength(2);
    expect(fake.addArgs[1]?.command_id).toBe(fake.addArgs[0]?.command_id);
  });

  it("uses a new command ID when the content changes after an unreachable error", async () => {
    const fake = seeded();
    fake.fail("projectAdd", "unreachable", "the server did not answer", 1);
    const { user, dialog } = await openDialog(fake);
    await pick(user, dialog, "scratch");
    await user.click(dialog.getByRole("button", { name: "Add project" }));
    await dialog.findByText("the server did not answer");
    await user.click(await screen.findByRole("button", { name: "Retry now" }));
    await waitFor(() => expect((dialog.getByRole("button", { name: "Add project" }) as HTMLButtonElement).disabled).toBe(false));
    await user.click(dialog.getByRole("checkbox"));
    await user.click(dialog.getByRole("button", { name: "Add project" }));
    await dialog.findByText("scratch is a project");
    expect(fake.addArgs[1]?.command_id).not.toBe(fake.addArgs[0]?.command_id);
  });
});
