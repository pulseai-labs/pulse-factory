// AC-7 (r1.s4.w3): the Needs you page. What it calls and when, what its cards show, how the request
// frames and the reads feed it, and that it sets no timer and opens no stream.
import type { RequestSummary } from "@pulse-factory/api-client";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../App";
import { parseStreamEvent } from "../transport/types";
import { advance, boot, type User } from "./chat-helpers";
import {
  FakeTransport,
  claimRecord,
  mergeAck,
  requestFrames,
  requestSummary,
  runSummary,
} from "./fake-transport";

/** An ISO time `h` hours before now: the page judges "the last 24 hours" by the clock. */
const ago = (h: number) => new Date(Date.now() - h * 3_600_000).toISOString();

const closed = (id: string, patch: Partial<RequestSummary> = {}) =>
  requestSummary(id, { status: "answered", answered_at: ago(1), verbs: [], ...patch });

/** A string that no source file holds: the page must show the run's own text (C12). */
const WAITING = "close spine zq-7731 once the moon is full";

const rail = () => within(screen.getByRole("navigation"));
const main = () => within(screen.getByRole("main"));
const card = (id: string) => within(screen.getByRole("article", { name: `Request ${id}` }));
const openList = () => within(main().getByRole("list", { name: "Open requests" }));

async function gotoNeedsYou(user: User): Promise<void> {
  await user.click(rail().getByRole("button", { name: "Needs you" }));
  await screen.findByRole("heading", { level: 2, name: "Needs you" });
}

async function openPage(fake: FakeTransport): Promise<User> {
  const user = await boot(fake);
  await gotoNeedsYou(user);
  return user;
}

async function frame(fake: FakeTransport, f: Parameters<FakeTransport["requestFrame"]>[0]) {
  await act(async () => {
    fake.requestFrame(f);
  });
}

/** The calls made since the app booted (the boot is paired, projectList, openStream). */
const since = (fake: FakeTransport, boot: number) => fake.calls.slice(boot);

describe("Needs you: what it calls", () => {
  it("makes exactly W9's calls when it opens: requests {all}, runList, and a show for each answerable request", async () => {
    const fake = new FakeTransport();
    fake.seedRequests([requestSummary("R-1"), mergeAck("R-2"), requestSummary("R-3", { kind: "gaps" })]);
    fake.bodies["R-1"] = { stop_text: "Which database do we use?" };
    const user = await boot(fake);
    const before = fake.calls.length;

    await gotoNeedsYou(user);
    await screen.findByText("Which database do we use?");
    await waitFor(() => expect(fake.count("show")).toBe(2));

    expect(since(fake, before).slice().sort()).toEqual(["requests", "runList", "show", "show"]);
    expect(fake.requestsArgs).toEqual([{ all: true }]);
    expect(fake.showArgs.map((a) => a.id).sort()).toEqual(["R-1", "R-3"]);
    expect(fake.count("confirmCode")).toBe(0);
    expect(fake.count("answer") + fake.count("ack") + fake.count("clear")).toBe(0);
  });

  it("opens no stream of its own", async () => {
    const fake = new FakeTransport();
    fake.seedRequests([requestSummary("R-1")]);
    const user = await boot(fake);
    const streams = fake.count("openStream");
    await gotoNeedsYou(user);
    await screen.findByRole("article", { name: "Request R-1" });
    expect(fake.count("openStream")).toBe(streams);
  });

  it("calls requests and runList once each on Refresh, and no show for a body it holds", async () => {
    const fake = new FakeTransport();
    fake.seedRequests([requestSummary("R-1"), mergeAck("R-2")]);
    const user = await openPage(fake);
    await screen.findByRole("article", { name: "Request R-2" });
    await waitFor(() => expect(fake.count("show")).toBe(1));
    const before = fake.calls.length;

    await user.click(main().getByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(fake.count("requests")).toBe(2));
    await waitFor(() => expect(fake.count("runList")).toBe(2));
    expect(since(fake, before).slice().sort()).toEqual(["requests", "runList"]);
  });
});

describe("Needs you: the cards", () => {
  it("lists the merge card first with its facts from the summary, and calls no show for it", async () => {
    const fake = new FakeTransport();
    fake.seedRequests([requestSummary("R-1"), mergeAck("R-2", { opened_at: "2026-11-10T12:00:00.000Z" })]);
    fake.bodies["R-1"] = { stop_text: "Which database do we use?" };
    await openPage(fake);
    await screen.findByText("Which database do we use?");

    const cards = openList().getAllByRole("article");
    expect(cards.map((c) => c.getAttribute("aria-label"))).toEqual(["Request R-2", "Request R-1"]);

    const merge = card("R-2");
    expect(merge.getByText("pulseai-labs/example#9")).toBeTruthy();
    expect(merge.getByText("main")).toBeTruthy();
    expect(merge.getByText("a".repeat(40))).toBeTruthy();
    expect(merge.getByRole("button", { name: "Confirm merge" })).toBeTruthy();
    expect(fake.showArgs.map((a) => a.id)).toEqual(["R-1"]);
    // No click so far: no code was asked for.
    expect(fake.count("confirmCode")).toBe(0);
  });

  it("shows a stop request's text from show, as text only", async () => {
    const fake = new FakeTransport();
    fake.seedRequests([requestSummary("R-1")]);
    fake.bodies["R-1"] = { stop_text: "Use <b>Postgres</b> or SQLite?" };
    await openPage(fake);

    const text = await screen.findByTestId("stop-text");
    expect(text.textContent).toBe("Use <b>Postgres</b> or SQLite?");
    expect(text.querySelector("b")).toBeNull();
    const c = card("R-1");
    expect(c.getByLabelText("Your answer")).toBeTruthy();
    const send = c.getByRole("button", { name: "Send answer" }) as HTMLButtonElement;
    expect(send.disabled).toBe(true);
  });

  it("shows each gap of a gaps request, and enables Send answer once there is text", async () => {
    const fake = new FakeTransport();
    fake.seedRequests([requestSummary("R-4", { kind: "gaps" })]);
    fake.bodies["R-4"] = {
      gaps: [
        { section: "§3", question: "Is the cache in scope?", severity: "blocking" },
        { section: "§5", question: "Which timeout?", severity: "nice-to-have" },
      ],
    };
    const user = await openPage(fake);

    const gaps = await screen.findByRole("list", { name: "Gaps of R-4" });
    expect(within(gaps).getAllByRole("listitem")).toHaveLength(2);
    expect(within(gaps).getByText("Is the cache in scope?")).toBeTruthy();
    expect(within(gaps).getByText("nice-to-have")).toBeTruthy();

    await user.type(card("R-4").getByLabelText("Your answer"), "yes");
    expect((card("R-4").getByRole("button", { name: "Send answer" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("shows a stop card with no more than its facts when the body holds neither text nor gaps", async () => {
    const fake = new FakeTransport();
    fake.seedRequests([requestSummary("R-1")]);
    await openPage(fake);
    await screen.findByRole("article", { name: "Request R-1" });
    await waitFor(() => expect(fake.count("show")).toBe(1));
    expect(screen.queryByTestId("stop-text")).toBeNull();
    expect(screen.queryByRole("list", { name: /^Gaps of/ })).toBeNull();
  });
});

describe("Needs you: the stream's frames", () => {
  it("adds a card for a request frame with no requests call and one show for it", async () => {
    const fake = new FakeTransport();
    fake.seedRequests([requestSummary("R-1")]);
    fake.bodies["R-1"] = { stop_text: "first?" };
    await openPage(fake);
    await screen.findByText("first?");
    await waitFor(() => expect(fake.count("show")).toBe(1));
    const requests = fake.count("requests");
    const runs = fake.count("runList");

    const next = requestSummary("R-5");
    fake.requestRows.push(next);
    fake.bodies["R-5"] = { stop_text: "second?" };
    await frame(fake, requestFrames.request(next));

    await screen.findByText("second?");
    expect(fake.count("requests")).toBe(requests);
    expect(fake.count("runList")).toBe(runs);
    expect(fake.showArgs.map((a) => a.id)).toEqual(["R-1", "R-5"]);
  });

  it("asks for a body again when a frame brings a new revision, and not for the same one twice", async () => {
    const fake = new FakeTransport();
    fake.seedRequests([requestSummary("R-1")]);
    fake.bodies["R-1"] = { stop_text: "first?" };
    await openPage(fake);
    await screen.findByText("first?");
    await frame(fake, requestFrames.request(requestSummary("R-1")));
    expect(fake.count("show")).toBe(1);

    fake.requestRows = [requestSummary("R-1", { revision: 2 })];
    fake.bodies["R-1"] = { stop_text: "second?" };
    await frame(fake, requestFrames.request(requestSummary("R-1", { revision: 2 })));
    await screen.findByText("second?");
    expect(fake.count("show")).toBe(2);
  });

  it("moves a request that a frame closes to Answered", async () => {
    const fake = new FakeTransport();
    fake.seedRequests([requestSummary("R-1"), requestSummary("R-2")]);
    await openPage(fake);
    await screen.findByRole("article", { name: "Request R-2" });

    await frame(fake, requestFrames.request(closed("R-1")));

    expect(screen.queryByRole("article", { name: "Request R-1" })).toBeNull();
    const answered = within(screen.getByRole("list", { name: "Answered" }));
    expect(answered.getByText("R-1", { selector: "strong" })).toBeTruthy();
    expect(answered.getByText("answered")).toBeTruthy();
  });

  it("drops, on a requests snapshot after a reconnect, a request that closed meanwhile", async () => {
    const fake = new FakeTransport();
    fake.seedRequests([requestSummary("R-1"), requestSummary("R-2")]);
    await openPage(fake);
    await screen.findByRole("article", { name: "Request R-2" });

    await frame(fake, requestFrames.requests([requestSummary("R-2")]));

    expect(screen.queryByRole("article", { name: "Request R-1" })).toBeNull();
    expect(screen.getByRole("article", { name: "Request R-2" })).toBeTruthy();
    expect(main().getByText("1 open")).toBeTruthy();
  });

  it("counts the open requests on the rail from frames alone, before the page ever opened", async () => {
    const fake = new FakeTransport();
    await boot(fake);
    const button = rail().getByRole("button", { name: "Needs you" });
    expect(button.querySelector(".rail-badge")).toBeNull();

    await frame(fake, requestFrames.requests([requestSummary("R-1"), mergeAck("R-2"), closed("R-3")]));

    const badge = button.querySelector(".rail-badge") as HTMLElement;
    expect(badge.textContent).toBe("2");
    expect(badge.getAttribute("title")).toBe("2 open");
    expect(fake.count("requests")).toBe(0);
    expect(fake.count("show")).toBe(0);

    await frame(fake, requestFrames.request(closed("R-1")));
    expect((button.querySelector(".rail-badge") as HTMLElement).textContent).toBe("1");
  });

  it("follows claim frames in the claim line", async () => {
    const fake = new FakeTransport();
    await openPage(fake);
    expect(main().getByText("No claim")).toBeTruthy();

    await frame(fake, requestFrames.claim(claimRecord({ generation: 2, holder: "ann@host" })));
    expect(main().getByText("ann@host, generation 2")).toBeTruthy();

    await frame(fake, requestFrames.claim(claimRecord({ generation: 2, holder: "ann@host", revoked: true })));
    expect(main().getByText("ann@host, generation 2, revoked")).toBeTruthy();

    await frame(fake, requestFrames.claim(null));
    expect(main().getByText("No claim")).toBeTruthy();
  });

  it("drops a request_frame that does not have the summary's shape and applies the next good one", async () => {
    const fake = new FakeTransport();
    await openPage(fake);

    // The native side delivers an event only if `parseStreamEvent` accepts it.
    for (const bad of [
      { type: "request", request: "oops" },
      { type: "request", request: { id: 7, revision: "x", status: 1 } },
      { type: "requests", requests: "nope", claim: null },
      { type: "claim", claim: "nope" },
    ]) {
      expect(parseStreamEvent({ event: "request_frame", frame: bad })).toBeNull();
    }
    const good = parseStreamEvent({ event: "request_frame", frame: requestFrames.request(requestSummary("R-8")) });
    expect(good).not.toBeNull();
    if (good) await act(async () => fake.emit(good));
    expect(await screen.findByRole("article", { name: "Request R-8" })).toBeTruthy();
  });
});

describe("Needs you: the reads", () => {
  it("fills Answered from the requests {all} read alone: 3 hours ago is in, 30 hours ago is not", async () => {
    const fake = new FakeTransport();
    fake.seedRequests([closed("R-1", { answered_at: ago(3) }), closed("R-2", { answered_at: ago(30) })]);
    await openPage(fake);

    const answered = within(await screen.findByRole("list", { name: "Answered" }));
    expect(answered.getByText("R-1", { selector: "strong" })).toBeTruthy();
    expect(answered.queryByText("R-2", { selector: "strong" })).toBeNull();
    // No frame was emitted.
    expect(fake.count("requests")).toBe(1);
  });

  it("shows Running, Waiting on you with the run's own text, Done and Halted from runList", async () => {
    const fake = new FakeTransport();
    fake.runs = [
      runSummary("R-1", { waiting: WAITING }),
      runSummary("R-2", { state: "ended", end_reason: "spine closed" }),
      runSummary("R-3", { halted_reason: "checks red" }),
      runSummary("R-4"),
    ];
    await openPage(fake);

    await screen.findByRole("article", { name: "Run R-1" });
    const run = (id: string) => within(screen.getByRole("article", { name: `Run ${id}` }));
    expect(run("R-1").getByText(`Waiting on you: ${WAITING}`)).toBeTruthy();
    expect(run("R-2").getByText("Done: spine closed")).toBeTruthy();
    expect(run("R-3").getByText("Halted: checks red")).toBeTruthy();
    expect(run("R-4").getByText("Running")).toBeTruthy();
  });

  it("shows one error line and Refresh when requests fails at open, and a good Refresh clears it", async () => {
    const fake = new FakeTransport();
    fake.seedRequests([requestSummary("R-1")]);
    fake.fail("requests", "server", "the store is busy");
    const user = await openPage(fake);

    const alert = await screen.findByRole("alert");
    expect(within(alert).getByText("the store is busy")).toBeTruthy();
    expect(screen.queryByRole("article", { name: "Request R-1" })).toBeNull();

    fake.heal("requests");
    await user.click(within(alert).getByRole("button", { name: "Refresh" }));
    expect(await screen.findByRole("article", { name: "Request R-1" })).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("shows one error line and Refresh when runList fails at open, and a good Refresh clears it", async () => {
    const fake = new FakeTransport();
    fake.runs = [runSummary("R-1")];
    fake.fail("runList", "refused", "no runs for you");
    const user = await openPage(fake);

    const alert = await screen.findByRole("alert");
    expect(within(alert).getByText("no runs for you")).toBeTruthy();
    expect(screen.queryByRole("article", { name: "Run R-1" })).toBeNull();

    fake.heal("runList");
    await user.click(within(alert).getByRole("button", { name: "Refresh" }));
    expect(await screen.findByRole("article", { name: "Run R-1" })).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("labels what it still shows after a failed read, and never shows it as current", async () => {
    const fake = new FakeTransport();
    fake.runs = [runSummary("R-1")];
    const user = await openPage(fake);
    await screen.findByRole("article", { name: "Run R-1" });

    fake.fail("runList", "server", "gone");
    await user.click(main().getByRole("button", { name: "Refresh" }));
    expect(await screen.findByText(/^Last read at /)).toBeTruthy();
    expect(screen.getByRole("alert").textContent).toContain("gone");
  });

  it("shows the 3c banner for an unauthorized requests, and no error line", async () => {
    const fake = new FakeTransport();
    // A down kind reaches the banner from any call; a call that works clears it, so fail both reads.
    fake.fail("requests", "unauthorized", "token revoked");
    fake.fail("runList", "unauthorized", "token revoked");
    await openPage(fake);

    expect(await screen.findByText(/This device is no longer paired with/)).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("shows a body error on the card, with Refresh, when show fails", async () => {
    const fake = new FakeTransport();
    fake.seedRequests([requestSummary("R-1")]);
    fake.bodies["R-1"] = { stop_text: "back again?" };
    fake.fail("show", "server", "no body now");
    const user = await openPage(fake);

    const alert = await screen.findByRole("alert");
    expect(within(alert).getByText("no body now")).toBeTruthy();

    fake.heal("show");
    await user.click(within(alert).getByRole("button", { name: "Refresh" }));
    expect(await screen.findByText("back again?")).toBeTruthy();
  });
});

describe("Needs you: no timer, no focus handler", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("makes no call on the page for an hour, blurred, focused, hidden or shown", async () => {
    const fake = new FakeTransport();
    fake.seedRequests([requestSummary("R-1"), mergeAck("R-2")]);
    fake.bodies["R-1"] = { stop_text: "still there?" };
    fake.runs = [runSummary("R-1", { waiting: WAITING })];
    render(<App transport={fake} />);
    await advance(0);
    fireEvent.click(rail().getByRole("button", { name: "Needs you" }));
    await advance(0);
    expect(screen.getByText("still there?")).toBeTruthy();
    const calls = [...fake.calls];
    expect(calls.filter((c) => c === "requests")).toHaveLength(1);

    await advance(30 * 60_000);
    await act(async () => {
      window.dispatchEvent(new Event("blur"));
      document.dispatchEvent(new Event("visibilitychange"));
      window.dispatchEvent(new Event("focus"));
      window.dispatchEvent(new Event("online"));
    });
    await advance(30 * 60_000);
    expect(fake.calls).toEqual(calls);
  });

  it("reads the requests and the runs once when the link comes back, and then no more", async () => {
    const fake = new FakeTransport();
    fake.seedRequests([requestSummary("R-1")]);
    render(<App transport={fake} />);
    await advance(0);
    fireEvent.click(rail().getByRole("button", { name: "Needs you" }));
    await advance(0);
    expect(fake.count("requests")).toBe(1);
    expect(fake.count("runList")).toBe(1);

    // Refresh meets a dead link: the 3c banner shows and its retry calls only `status`.
    fake.fail("requests", "unreachable", "no answer", 1);
    fake.fail("runList", "unreachable", "no answer", 1);
    fake.fail("status", "unreachable", "no answer");
    fireEvent.click(main().getByRole("button", { name: "Refresh" }));
    await advance(0);
    expect(screen.getByText(/Not connected to/)).toBeTruthy();
    expect(fake.count("requests")).toBe(2);
    expect(fake.count("runList")).toBe(2);

    fake.heal();
    await advance(60_000);
    expect(screen.queryByText(/Not connected to/)).toBeNull();
    expect(fake.count("requests")).toBe(3);
    expect(fake.count("runList")).toBe(3);

    const calls = [...fake.calls];
    await advance(60 * 60_000);
    expect(fake.calls).toEqual(calls);
  });
});
