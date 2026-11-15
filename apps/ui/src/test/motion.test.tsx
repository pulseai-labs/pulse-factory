// The motion of the approved canvas (patch `fix/ui-canvas-motion`, 2026-11-15). 11a, 11b and 11e put the sidebar
// and the dock each in a frame: `flex:none; display:flex; overflow:hidden; width:<open ? W : 0>; transition: width
// 280ms var(--ease-standard)`, and the dock's frame also has `justify-content:flex-end`. The panel stays mounted
// at its own width and the frame clips it, so the main column reflows as the width moves. A page change has no
// transition (the operator checked the canvas, 2026-11-15).
//
// jsdom runs no CSS transition, so these tests read what decides one: that a closed panel is still mounted and is
// inert and hidden, that each frame carries its state from its first commit and is never restyled on mount, that
// a page change drops and mounts frames instead of animating them, and the rules in the style sheets.
// `e2e/motion.spec.ts` watches the transitions themselves in Chromium.
import { act, render, screen, waitFor, within } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import type { RequestSummary } from "@pulse-factory/api-client";
import { App } from "../App";
import { useRequestBodies } from "../needs-you/useRequestBodies";
import { SidePanel } from "../shell/SidePanel";
import { ChatProvider } from "../state/chat";
import { ConnectionProvider, useConnection } from "../state/connection";
import { RequestsProvider } from "../state/requests";
import { alpha, boot, bootChat, push, seeded } from "./chat-helpers";
import { ADDRESS, FakeTransport, device, frames, mergeAck, project, requestFrames, requestSummary } from "./fake-transport";

type PanelName = "sidebar" | "dock";

const frame = (name: PanelName) => document.querySelector<HTMLElement>(`[data-panel="${name}"]`);
const bar = () => within(screen.getByRole("banner"));
const rail = () => within(screen.getByRole("navigation"));
const sidebarRegion = () => screen.queryByRole("region", { name: "Sidebar" });
const dockRegion = () => screen.queryByRole("complementary", { name: "Needs you" });

function expectClosed(name: PanelName): HTMLElement {
  const el = frame(name);
  expect(el, `the ${name}'s frame is mounted`).not.toBeNull();
  const f = el as HTMLElement;
  expect(f.getAttribute("data-open"), `the ${name} is closed`).toBe("false");
  expect(f.hasAttribute("inert"), `the closed ${name} is inert`).toBe(true);
  expect(f.getAttribute("aria-hidden"), `the closed ${name} is hidden from the accessibility tree`).toBe("true");
  return f;
}

function expectOpen(name: PanelName): HTMLElement {
  const el = frame(name);
  expect(el, `the ${name}'s frame is mounted`).not.toBeNull();
  const f = el as HTMLElement;
  expect(f.getAttribute("data-open"), `the ${name} is open`).toBe("true");
  expect(f.hasAttribute("inert"), `the open ${name} is not inert`).toBe(false);
  expect(f.hasAttribute("aria-hidden"), `the open ${name} is in the accessibility tree`).toBe(false);
  return f;
}

/** Every attribute change on a panel's frame, from the moment it is called until `stop`. */
function watchFrames(): { stop(): MutationRecord[] } {
  const seen: MutationRecord[] = [];
  const observer = new MutationObserver((records) => seen.push(...records));
  observer.observe(document.body, { subtree: true, attributes: true, attributeOldValue: true });
  return {
    stop() {
      seen.push(...observer.takeRecords());
      observer.disconnect();
      return seen.filter((r) => r.target instanceof HTMLElement && r.target.hasAttribute("data-panel"));
    },
  };
}

describe("the sidebar's frame (11a, 11b)", () => {
  it("keeps the sidebar mounted when it closes, inert and out of the accessibility tree, and ⌘B opens it again", async () => {
    const user = await boot(new FakeTransport({ projects: [project(1, "alpha")] }));
    expectOpen("sidebar");
    expect(sidebarRegion()).not.toBeNull();

    await user.keyboard("{Meta>}b{/Meta}");
    const closed = expectClosed("sidebar");
    expect(sidebarRegion()).toBeNull();
    // Still mounted, with its content: the frame clips it, nothing in it changes.
    const section = closed.querySelector("section.sidebar");
    expect(section).not.toBeNull();
    expect(within(section as HTMLElement).getByText("alpha", { exact: true })).toBeTruthy();

    await user.keyboard("{Meta>}b{/Meta}");
    expectOpen("sidebar");
    expect(sidebarRegion()).not.toBeNull();
    // The same element came back: it was never unmounted.
    expect(frame("sidebar")?.querySelector("section.sidebar")).toBe(section);
  });

  it("closes and opens with the title bar's toggle too", async () => {
    const user = await boot(new FakeTransport({ projects: [project(1, "alpha")] }));
    await user.click(bar().getByRole("button", { name: "Show or hide the sidebar" }));
    expectClosed("sidebar");
    await user.click(bar().getByRole("button", { name: "Show or hide the sidebar" }));
    expectOpen("sidebar");
  });

  it("moves the focus out of the sidebar when it closes with the focus inside", async () => {
    const user = await boot(new FakeTransport({ projects: [project(1, "alpha")] }));
    const search = screen.getByRole("searchbox", { name: "Search sessions" });
    await user.click(search);
    expect(document.activeElement).toBe(search);

    await user.keyboard("{Meta>}b{/Meta}");
    const closed = expectClosed("sidebar");
    expect(closed.contains(document.activeElement)).toBe(false);
    // ⌘B typed nothing into the field.
    expect((search as HTMLInputElement).value).toBe("");
  });

  it("makes no call while it is closed, and reads the run list each time it opens", async () => {
    window.localStorage.setItem("pf.sidebar", "closed");
    const fake = new FakeTransport({ projects: [project(1, "alpha")] });
    const user = await boot(fake);
    expectClosed("sidebar");
    expect(fake.count("runList")).toBe(0);

    await user.keyboard("{Meta>}b{/Meta}");
    expectOpen("sidebar");
    await waitFor(() => expect(fake.count("runList")).toBe(1));
    await user.keyboard("{Meta>}b{/Meta}");
    expect(fake.count("runList")).toBe(1);
    await user.keyboard("{Meta>}b{/Meta}");
    await waitFor(() => expect(fake.count("runList")).toBe(2));
  });
});

describe("the dock's frame (11a, 11b, 11e)", () => {
  it("keeps the dock mounted while it is closed, inert and hidden, and the pill opens and closes it", async () => {
    const user = await boot(new FakeTransport());
    const closed = expectClosed("dock");
    expect(dockRegion()).toBeNull();
    const aside = closed.querySelector("aside.dock");
    expect(aside).not.toBeNull();

    await user.click(bar().getByTitle("Show or hide Needs you"));
    expectOpen("dock");
    expect(dockRegion()).not.toBeNull();

    await user.click(bar().getByTitle("Show or hide Needs you"));
    expectClosed("dock");
    expect(dockRegion()).toBeNull();
    expect(frame("dock")?.querySelector("aside.dock")).toBe(aside);
  });

  it("makes no call while it is closed, and reads the bodies of the open requests when it opens", async () => {
    const fake = new FakeTransport();
    const user = await boot(fake);
    await act(async () => {
      fake.requestFrame(requestFrames.requests([requestSummary("R-1"), mergeAck("R-2")], null));
    });
    expectClosed("dock");
    expect(fake.count("show")).toBe(0);

    await user.click(bar().getByTitle("Show or hide Needs you"));
    expectOpen("dock");
    await waitFor(() => expect(fake.count("show")).toBe(2));
    expect(fake.showArgs.map((a) => a.id).sort()).toEqual(["R-1", "R-2"]);
  });
});

describe("a body whose `show` failed on a down link (PR #65, round 1)", () => {
  // A `show` that fails as `unreachable` keeps its key and draws no card error: the banner reports the link. Before
  // this patch a closed dock unmounted, so its next open read the body again. A dock that stays mounted must ask
  // again itself: when the link comes back, and when it opens. A closed dock still reads nothing.
  it("the open dock reads it again when the link comes back", async () => {
    window.localStorage.setItem("pf.dock", "open");
    const fake = new FakeTransport();
    fake.seedRequests([requestSummary("R-1")]);
    fake.bodies["R-1"] = { stop_text: "Which database do we use?" };
    fake.fail("show", "unreachable", "the bridge is gone");
    const user = await boot(fake);
    await act(async () => {
      fake.requestFrame(requestFrames.requests([requestSummary("R-1")], null));
    });
    await waitFor(() => expect(fake.count("show")).toBe(1));
    const dock = () => within(screen.getByRole("complementary", { name: "Needs you" }));
    expect(dock().queryByText("Which database do we use?")).toBeNull();

    fake.heal("show");
    await user.click(await screen.findByRole("button", { name: "Retry now" }));
    await waitFor(() => expect(fake.count("show")).toBe(2));
    expect(await dock().findByText("Which database do we use?")).toBeTruthy();
  });

  const R1 = requestSummary("R-1");
  const WANT: RequestSummary[] = [R1];

  function Probe(props: { active: boolean }) {
    const bodies = useRequestBodies(WANT, props.active);
    const { link, retryNow } = useConnection();
    return (
      <>
        <span data-testid="body">{bodies.detailOf(R1) ? "held" : "missing"}</span>
        <span data-testid="link">{link.state}</span>
        <button type="button" onClick={retryNow}>
          retry the link
        </button>
      </>
    );
  }

  function mount(fake: FakeTransport, active: boolean) {
    const tree = (on: boolean) => (
      <ConnectionProvider transport={fake} server={{ address: ADDRESS, device: device() }} onForgotten={() => {}} onPairAgain={() => {}}>
        <ChatProvider transport={fake}>
          <RequestsProvider>
            <Probe active={on} />
          </RequestsProvider>
        </ChatProvider>
      </ConnectionProvider>
    );
    const view = render(tree(active));
    return { set: (on: boolean) => view.rerender(tree(on)) };
  }

  const settle = () => act(async () => {});

  it("an inactive surface reads nothing, and reads when it becomes active", async () => {
    const fake = new FakeTransport();
    fake.seedRequests([R1]);
    const probe = mount(fake, false);
    await settle();
    expect(fake.count("show")).toBe(0);
    probe.set(true);
    await waitFor(() => expect(screen.getByTestId("body").textContent).toBe("held"));
    expect(fake.count("show")).toBe(1);
  });

  it("a link that comes back while the surface is inactive starts no read; the surface reads when it becomes active", async () => {
    const fake = new FakeTransport();
    fake.seedRequests([R1]);
    fake.fail("show", "unreachable", "the bridge is gone");
    const probe = mount(fake, true);
    await waitFor(() => expect(fake.count("show")).toBe(1));
    await settle();
    probe.set(false);
    fake.heal("show");
    await act(async () => {
      screen.getByRole("button", { name: "retry the link" }).click();
    });
    await settle();
    expect(screen.getByTestId("link").textContent).toBe("up");
    expect(fake.count("show")).toBe(1);

    probe.set(true);
    await waitFor(() => expect(screen.getByTestId("body").textContent).toBe("held"));
    expect(fake.count("show")).toBe(2);
  });

  it("a surface that opens asks again, even when the link came back without a change it could see", async () => {
    // The mount's `status()` answers in the same turn as the failed `show`, so the link reads up throughout.
    const fake = new FakeTransport();
    fake.seedRequests([R1]);
    fake.fail("show", "unreachable", "the bridge is gone", 1);
    const probe = mount(fake, true);
    await waitFor(() => expect(fake.count("show")).toBe(1));
    await settle();
    expect(screen.getByTestId("body").textContent).toBe("missing");
    probe.set(false);
    await settle();
    probe.set(true);
    await waitFor(() => expect(screen.getByTestId("body").textContent).toBe("held"));
    expect(fake.count("show")).toBe(2);
  });
});

describe("a frame's focus rule (SidePanel)", () => {
  it.each(["sidebar", "dock"] as const)("the %s's frame sends the focus out when it closes with the focus inside", (name) => {
    const view = render(
      <SidePanel name={name} open>
        <button type="button">inside</button>
      </SidePanel>,
    );
    const inside = screen.getByRole("button", { name: "inside" });
    act(() => inside.focus());
    expect(document.activeElement).toBe(inside);

    view.rerender(
      <SidePanel name={name} open={false}>
        <button type="button">inside</button>
      </SidePanel>,
    );
    expectClosed(name);
    expect(frame(name)?.contains(document.activeElement)).toBe(false);
  });

  it("leaves the focus where it is when it closes with the focus outside", () => {
    const view = render(
      <>
        <button type="button">outside</button>
        <SidePanel name="sidebar" open>
          <button type="button">inside</button>
        </SidePanel>
      </>,
    );
    const outside = screen.getByRole("button", { name: "outside" });
    act(() => outside.focus());
    view.rerender(
      <>
        <button type="button">outside</button>
        <SidePanel name="sidebar" open={false}>
          <button type="button">inside</button>
        </SidePanel>
      </>,
    );
    expect(document.activeElement).toBe(outside);
  });
});

describe("no animation on first paint, and none on a page change", () => {
  it("draws the persisted states in the first commit: neither frame changes after it mounts", async () => {
    window.localStorage.setItem("pf.sidebar", "closed");
    window.localStorage.setItem("pf.dock", "open");
    const watch = watchFrames();
    render(<App transport={new FakeTransport({ projects: [project(1, "alpha")] })} />);
    await screen.findByRole("navigation");
    await waitFor(() => expect(frame("dock")).not.toBeNull());
    expectClosed("sidebar");
    expectOpen("dock");
    // A transition needs a style change on a mounted element. Each frame was inserted in its persisted state
    // and none of its attributes changed after that, so there is nothing to animate on the first paint.
    expect(watch.stop().map((r) => `${(r.target as HTMLElement).dataset.panel} ${r.attributeName}`)).toEqual([]);
  });

  it("drops both frames on a focused page and mounts them again, in their state, on the next page", async () => {
    window.localStorage.setItem("pf.dock", "open");
    const user = await boot(new FakeTransport({ projects: [project(1, "alpha")] }));
    expectOpen("sidebar");
    expectOpen("dock");

    await user.click(rail().getByRole("button", { name: "Settings" }));
    expect(frame("sidebar")).toBeNull();
    expect(frame("dock")).toBeNull();

    const watch = watchFrames();
    await user.click(bar().getByRole("button", { name: "Back" }));
    expectOpen("sidebar");
    expectOpen("dock");
    expect(watch.stop()).toEqual([]);
  });
});

describe("the chat header's avatar takes the session's state (design-decisions.md, 9a)", () => {
  const head = () => screen.getByRole("group", { name: "Chat header" }).querySelector(".ui-avatar");

  it.each([
    ["working", "working"],
    ["needs_you", "needs_you"],
    ["idle", "idle"],
  ] as const)("a %s session draws a %s avatar", async (state, drawn) => {
    await bootChat(seeded({ sessions: [alpha({ state })] }));
    expect(head()?.getAttribute("data-state")).toBe(drawn);
  });

  it("an ended session draws an idle avatar", async () => {
    const fake = seeded({ sessions: [alpha({ state: "working" })] });
    await bootChat(fake);
    expect(head()?.getAttribute("data-state")).toBe("working");
    await push(fake, frames.session(alpha({ state: "working", ended_at: "2026-11-09T11:00:00Z", updated_at: "2026-11-09T11:00:00Z" })));
    expect(head()?.getAttribute("data-state")).toBe("idle");
  });
});

const SRC = join(__dirname, "..");
const read = (path: string) => readFileSync(join(SRC, path), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");

/** The declarations of the first rule whose selector is exactly `selector`, outside any `@media` block. */
function rule(css: string, selector: string): string {
  const top = css.replace(/@media[^{]*\{(?:[^{}]*\{[^{}]*\})*[^{}]*\}/g, "");
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const m = new RegExp(`(?:^|\\})\\s*${escaped}\\s*\\{([^}]*)\\}`).exec(top);
  return (m?.[1] ?? "").replace(/\s+/g, " ").trim();
}

/** The text of every `@media (prefers-reduced-motion: reduce)` block, whitespace folded. */
function reducedMotion(css: string): string {
  return [...css.matchAll(/@media\s*\(prefers-reduced-motion:\s*reduce\)\s*\{((?:[^{}]*\{[^{}]*\})*[^{}]*)\}/g)]
    .map((m) => (m[1] ?? "").replace(/\s+/g, " "))
    .join(" ");
}

describe("the style rules of the motion", () => {
  const tokens = read("styles/tokens.css");
  const shell = read("shell/shell.css");
  const app = read("styles/app.css");

  it("uses the canvas's motion tokens: 280 ms and 120 ms, and the standard curve (tokens/motion.css)", () => {
    expect(tokens).toMatch(/--dur-slow:\s*280ms;/);
    expect(tokens).toMatch(/--dur-fast:\s*120ms;/);
    expect(tokens).toMatch(/--ease-standard:\s*cubic-bezier\(0\.2, 0, 0, 1\);/);
  });

  it("gives each frame the canvas's rule: no flex, a flex row, clipped, and its width animated", () => {
    const base = rule(shell, ".panel");
    for (const part of ["flex: none;", "display: flex;", "overflow: hidden;", "transition: width var(--dur-slow) var(--ease-standard);"]) {
      expect(base, `.panel has ${part}`).toContain(part);
    }
    expect(rule(shell, ".panel-sidebar")).toContain("width: var(--sidebar-w);");
    expect(rule(shell, ".panel-dock")).toContain("width: var(--dock-w);");
    expect(rule(shell, ".panel-dock")).toContain("justify-content: flex-end;");
    expect(rule(shell, '.panel[data-open="false"]')).toContain("width: 0;");
  });

  it("keeps each panel at its own width inside the frame", () => {
    for (const [selector, width] of [
      [".sidebar", "--sidebar-w"],
      [".dock", "--dock-w"],
    ] as const) {
      expect(rule(shell, selector)).toContain("flex: none;");
      expect(rule(shell, selector)).toContain(`width: var(${width});`);
    }
  });

  it("animates the switch as 11a does: the track's background and the knob's left, 120 ms", () => {
    expect(rule(app, ".switch")).toContain("transition: background var(--dur-fast);");
    expect(rule(app, ".switch::after")).toContain("transition: left var(--dur-fast);");
  });

  it("turns every new transition off under reduced motion", () => {
    expect(reducedMotion(shell)).toMatch(/\.panel \{ transition: none; \}/);
    expect(reducedMotion(app)).toMatch(/\.switch, \.switch::after \{ transition: none; \}/);
  });

  it("gives no page part a transition or an animation, and names no route duration", () => {
    const files = ["styles/app.css", "shell/shell.css", "ui/ui.css", "styles/add-project.css", "styles/needs-you.css", "styles/agents.css", "styles/settings.css"];
    for (const file of files) {
      const css = read(file);
      expect(css.includes("--dur-slower"), `${file} names --dur-slower`).toBe(false);
      for (const m of css.matchAll(/([^{}]+)\{([^}]*)\}/g)) {
        const selectors = (m[1] ?? "").split(",").map((s) => s.trim());
        const page = selectors.some((s) => /^\.(app|body|main|page|column)(\s|$|>|:)/.test(s) || s === ".main > *");
        if (page) expect(/\b(transition|animation)\s*:/.test(m[2] ?? ""), `${file}: ${m[1]?.trim()}`).toBe(false);
      }
    }
  });
});
