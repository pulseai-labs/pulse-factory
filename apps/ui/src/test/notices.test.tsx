// AC-3 (r2.s11.w5): the told signal in the UI (12f; Q9). A `notice` frame of the one stream is gathered into a
// batch (300 ms after its last frame, at most 1,000 ms after its first, with one timer that stops itself); a
// batch shows one notification for its `opened` notices (a count form for several) and acknowledges once with
// the outcome. Only a server that sent a notice frame is acknowledged. A blocked outcome shows the banner on
// the page and in the dock. Nothing of a notice goes to browser storage, the console or a URL.
import { act, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../App";
import { parseStreamEvent } from "../transport/types";
import { FakeNotifier } from "./fake-notifier";
import { FakeTransport, noticeRecord, project, requestSummary } from "./fake-transport";

beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

async function advance(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

async function boot(opts: { notifier?: FakeNotifier } = {}) {
  const fake = new FakeTransport({ projects: [project(1, "alpha")] });
  fake.statusServer = { id: "0".repeat(64), name: "draco-desk" };
  const notifier = opts.notifier ?? new FakeNotifier();
  render(<App transport={fake} notifier={notifier} />);
  await advance(0);
  return { fake, notifier };
}

async function notice(fake: FakeTransport, ...records: Array<ReturnType<typeof noticeRecord>>) {
  await act(async () => {
    for (const r of records) fake.noticeFrame(r);
  });
}

/** Let a batch end: 300 ms after its last frame. */
const endBatch = () => advance(300);

describe("the notice_frame event", () => {
  it("is parsed when its frame is a notice with a number position, a string event and a string request", () => {
    const frame = { type: "notice", notice: noticeRecord(4) };
    expect(parseStreamEvent({ event: "notice_frame", frame })).toEqual({ event: "notice_frame", frame });
  });

  it.each([
    ["no frame", { event: "notice_frame" }],
    ["a frame of another type", { event: "notice_frame", frame: { type: "requests", requests: [], claim: null } }],
    ["a notice with no position", { event: "notice_frame", frame: { type: "notice", notice: { event: "opened", request: "R-1" } } }],
    ["a notice with a string position", { event: "notice_frame", frame: { type: "notice", notice: { position: "1", event: "opened", request: "R-1" } } }],
    ["a notice with no request", { event: "notice_frame", frame: { type: "notice", notice: { position: 1, event: "opened" } } }],
    ["no notice", { event: "notice_frame", frame: { type: "notice" } }],
  ])("drops %s", (_name, value) => {
    expect(parseStreamEvent(value)).toBeNull();
  });

  it("drops another shape with the stream open", async () => {
    const { fake, notifier } = await boot();
    await act(async () => {
      fake.emit({ event: "notice_frame", frame: { type: "notice", notice: { position: "x" } } } as never);
    });
    await endBatch();
    expect(fake.current()).toBeDefined();
    expect(notifier.shown).toHaveLength(0);
    expect(fake.count("noticeAck")).toBe(0);
  });
});

describe("one batch", () => {
  it("calls no `noticeAck` before the first notice frame, in any state of the app", async () => {
    const { fake } = await boot();
    await advance(60_000);
    expect(fake.count("noticeAck")).toBe(0);
  });

  const KINDS: Array<[kind: string, what: string]> = [
    ["stop", "u1 asks a question"],
    ["gaps", "u1 found gaps"],
    ["red_gate", "u1’s gate failed"],
    ["park", "u1 is parked"],
    ["stall", "u1 stalled"],
    ["seat_lost", "u1 lost its seat"],
    ["merge_failed", "u1’s merge failed"],
    ["merge_ack", "u1 is ready to merge"],
    ["something_new", "a request opened"],
  ];

  it.each(KINDS)("shows one notification for an opened %s notice: `<project> · %s`, then the text and the request ID", async (kind, what) => {
    const { fake, notifier } = await boot();
    await notice(fake, noticeRecord(1, { kind, text: "The controller's text." }));
    expect(notifier.shown).toHaveLength(0);
    await endBatch();
    expect(notifier.shown).toEqual([{ title: `alpha · ${what}`, body: "The controller's text. R-12", request: "R-12" }]);
  });

  it("names the run ID where the notice has no project, and the run ID in place of a missing unit", async () => {
    const { fake, notifier } = await boot();
    const { project: _p, unit: _u, ...bare } = noticeRecord(1, { kind: "stall" });
    await notice(fake, bare);
    await endBatch();
    expect(notifier.shown[0]?.title).toBe("R-1 · R-1 stalled");
  });

  it("shows one notification that counts several opened notices, with the newest", async () => {
    const { fake, notifier } = await boot();
    await notice(
      fake,
      noticeRecord(1, { request: "R-10", text: "First." }),
      noticeRecord(2, { request: "R-11", text: "Second." }),
      noticeRecord(3, { request: "R-12", text: "Newest one." }),
    );
    await endBatch();
    expect(notifier.shown).toEqual([{ title: "Pulse Factory · 3 new requests", body: "Newest: alpha · Newest one. R-12", request: "R-12" }]);
    expect(fake.noticeAckArgs).toEqual([{ position: 3, outcome: "shown" }]);
  });

  it("shows none for a revised or a closed notice, and acknowledges them", async () => {
    const { fake, notifier } = await boot();
    await notice(fake, noticeRecord(1, { event: "revised", revision: 2 }), noticeRecord(2, { event: "closed", request: "R-13" }));
    await endBatch();
    expect(notifier.shown).toHaveLength(0);
    expect(fake.noticeAckArgs).toEqual([{ position: 2, outcome: "shown" }]);
  });

  it("shows none for an opened notice whose request the batch closes, and acknowledges both", async () => {
    const { fake, notifier } = await boot();
    await notice(fake, noticeRecord(1, { request: "R-12" }), noticeRecord(2, { event: "closed", request: "R-12" }));
    await endBatch();
    expect(notifier.shown).toHaveLength(0);
    expect(fake.noticeAckArgs).toEqual([{ position: 2, outcome: "shown" }]);
  });

  it("shows none for an opened notice whose request the requests state holds as closed", async () => {
    const { fake, notifier } = await boot();
    await act(async () => {
      fake.requestFrame({ type: "request", request: requestSummary("R-12", { status: "answered", answered_at: "2026-11-10T10:20:00.000Z", verbs: [] }) });
    });
    await notice(fake, noticeRecord(1, { request: "R-12" }));
    await endBatch();
    expect(notifier.shown).toHaveLength(0);
    expect(fake.noticeAckArgs).toEqual([{ position: 1, outcome: "shown" }]);
  });

  it("calls `noticeAck` once for a batch, at its highest position, with `shown`", async () => {
    const { fake } = await boot();
    await notice(fake, noticeRecord(5, { request: "R-10" }), noticeRecord(9, { request: "R-11" }), noticeRecord(7, { request: "R-12" }));
    await endBatch();
    expect(fake.noticeAckArgs).toEqual([{ position: 9, outcome: "shown" }]);
    expect(fake.count("noticeAck")).toBe(1);
  });

  it("makes no call for a frame at or below the acknowledged position, and no second notification", async () => {
    const { fake, notifier } = await boot();
    await notice(fake, noticeRecord(4));
    await endBatch();
    expect(fake.count("noticeAck")).toBe(1);
    await notice(fake, noticeRecord(4, { sends: 2 }), noticeRecord(3, { request: "R-9" }));
    await endBatch();
    expect(fake.count("noticeAck")).toBe(1);
    expect(notifier.shown).toHaveLength(1);
  });

  it("acknowledges a notice that is sent again after a failed `noticeAck`, with no second notification", async () => {
    const { fake, notifier } = await boot();
    fake.fail("noticeAck", "server", "the server failed", 1);
    await notice(fake, noticeRecord(4));
    await endBatch();
    expect(notifier.shown).toHaveLength(1);
    expect(fake.count("noticeAck")).toBe(1);
    // The app does not try again by itself.
    await advance(60_000);
    expect(fake.count("noticeAck")).toBe(1);
    // The server sends it again: the next batch acknowledges it and shows nothing more.
    await notice(fake, noticeRecord(4, { sends: 2 }));
    await endBatch();
    expect(fake.noticeAckArgs).toEqual([
      { position: 4, outcome: "shown" },
      { position: 4, outcome: "shown" },
    ]);
    expect(notifier.shown).toHaveLength(1);
  });

  it("ends the batch 300 ms after its last frame", async () => {
    const { fake, notifier } = await boot();
    await notice(fake, noticeRecord(1, { request: "R-10" }));
    await advance(250);
    await notice(fake, noticeRecord(2, { request: "R-11" }));
    await advance(250);
    expect(fake.count("noticeAck")).toBe(0);
    await advance(50);
    expect(fake.noticeAckArgs).toEqual([{ position: 2, outcome: "shown" }]);
    expect(notifier.shown).toHaveLength(1);
  });

  it("ends the batch at most 1,000 ms after its first frame, and stops its timer", async () => {
    const { fake } = await boot();
    for (let i = 1; i <= 8; i += 1) {
      await notice(fake, noticeRecord(i, { request: `R-${i}` }));
      await advance(200);
    }
    // 1,600 ms of frames 200 ms apart: the first batch ended at 1,000 ms; the rest is the next.
    expect(fake.count("noticeAck")).toBe(1);
    expect(fake.noticeAckArgs[0]?.position).toBeLessThan(8);
    await endBatch();
    expect(fake.count("noticeAck")).toBe(2);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("sets no timer while no frame arrives, and none stays after a batch", async () => {
    const { fake } = await boot();
    const idle = vi.getTimerCount();
    await advance(10_000);
    expect(vi.getTimerCount()).toBe(idle);
    await notice(fake, noticeRecord(1));
    await endBatch();
    expect(vi.getTimerCount()).toBe(idle);
  });
});

describe("the blocked state", () => {
  const PAGE_LINE = "macOS is blocking notifications from Pulse Factory, so new requests only show here. draco-desk lists this Mac as showing none.";
  const DOCK_LINE = "macOS is blocking Pulse Factory’s notifications. New requests only show here.";
  const dock = () => document.querySelector("aside.dock") as HTMLElement;

  async function toNeedsYou() {
    await act(async () => {
      screen.getByRole("button", { name: "Needs you" }).click();
    });
    await advance(0);
  }

  it("acknowledges `blocked` when the notifier says so, and shows the banner in the dock and on the page", async () => {
    const notifier = new FakeNotifier();
    notifier.outcome = "blocked";
    const { fake } = await boot({ notifier });
    await notice(fake, noticeRecord(1));
    await endBatch();
    expect(fake.noticeAckArgs).toEqual([{ position: 1, outcome: "blocked" }]);
    expect(within(dock()).getByText(DOCK_LINE)).toBeTruthy();
    await toNeedsYou();
    expect(screen.getByText(PAGE_LINE)).toBeTruthy();
    // The two buttons are later-release controls.
    for (const label of ["Open System Settings"]) {
      expect(screen.getByText(label).closest("[data-not-built]")?.getAttribute("aria-disabled")).toBe("true");
    }
  });

  it("acknowledges `blocked` when the notification fails (the notifier throws)", async () => {
    const notifier = new FakeNotifier();
    notifier.outcome = "throw";
    const { fake } = await boot({ notifier });
    await notice(fake, noticeRecord(1));
    await endBatch();
    expect(fake.noticeAckArgs).toEqual([{ position: 1, outcome: "blocked" }]);
    expect(within(dock()).getByText(DOCK_LINE)).toBeTruthy();
  });

  it("removes the banner when a later acknowledgement carries `shown`", async () => {
    const notifier = new FakeNotifier();
    notifier.outcome = "blocked";
    const { fake } = await boot({ notifier });
    await notice(fake, noticeRecord(1));
    await endBatch();
    expect(within(dock()).queryByText(DOCK_LINE)).not.toBeNull();
    notifier.outcome = "shown";
    await notice(fake, noticeRecord(2, { request: "R-13" }));
    await endBatch();
    expect(fake.noticeAckArgs.at(-1)).toEqual({ position: 2, outcome: "shown" });
    expect(within(dock()).queryByText(DOCK_LINE)).toBeNull();
  });

  it("shows no banner before any acknowledgement", async () => {
    await boot();
    expect(within(dock()).queryByText(DOCK_LINE)).toBeNull();
  });
});

describe("a click on a notification", () => {
  it("opens Needs you and scrolls to the card of the request it names", async () => {
    const scroll = vi.fn();
    Element.prototype.scrollIntoView = scroll;
    const { fake, notifier } = await boot();
    fake.seedRequests([requestSummary("R-11"), requestSummary("R-12")]);
    await act(async () => {
      fake.requestFrame({ type: "requests", requests: fake.requestRows, claim: null });
    });
    await act(async () => {
      notifier.click("R-12");
    });
    await advance(0);
    expect(document.getElementById("needs-you-title")).not.toBeNull();
    const card = screen.getByRole("article", { name: "Request R-12" });
    expect(scroll).toHaveBeenCalledTimes(1);
    expect(scroll.mock.contexts[0]).toBe(card);
  });

  it("opens Needs you when the click names no request, and scrolls nowhere", async () => {
    const scroll = vi.fn();
    Element.prototype.scrollIntoView = scroll;
    const { notifier } = await boot();
    await act(async () => {
      notifier.click(null);
    });
    await advance(0);
    expect(document.getElementById("needs-you-title")).not.toBeNull();
    expect(scroll).not.toHaveBeenCalled();
  });
});

describe("what a notice leaves behind", () => {
  it("puts nothing of a notice in browser storage or the console", async () => {
    const log = vi.spyOn(console, "log").mockImplementation(() => {});
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    const { fake } = await boot();
    await notice(fake, noticeRecord(1, { text: "SECRET-NOTICE-TEXT" }));
    await endBatch();
    for (const store of [window.localStorage, window.sessionStorage]) {
      for (let i = 0; i < store.length; i += 1) {
        const key = store.key(i) as string;
        expect(`${key}=${store.getItem(key) ?? ""}`).not.toContain("SECRET-NOTICE-TEXT");
        expect(`${key}=${store.getItem(key) ?? ""}`).not.toMatch(/notice/i);
      }
    }
    for (const spy of [log, warn, error]) {
      expect(JSON.stringify(spy.mock.calls)).not.toContain("SECRET-NOTICE-TEXT");
    }
    expect(window.location.href).not.toContain("SECRET");
  });
});
