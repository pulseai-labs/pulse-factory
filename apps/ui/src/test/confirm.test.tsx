// The confirm dialog as a component (r1.s4.w3, kept by r1.s5.w4). One act is two clicks: the first asked for a
// one-use confirm code, the dialog shows what it binds, the second spends it. Cancel and Escape spend
// nothing, a refusal says why, and the code is in memory only.
//
// `r1.s5.w4` (ruling G1) took the dialog off the Needs you page: the page's acts are one press
// (`needs-you-cards.test.tsx`). `ConfirmDialog.tsx` stays in the tree with its props and its `refusalLine`,
// which the one-press hook imports, so this file tests the component on its own.
//
// Known limit (Q2, C1): the code proves the device, the freshness and the head, not that a person pressed
// the button. These tests check the form of the control, and claim no more.
import type { RequestActed, RequestSummary } from "@pulse-factory/api-client";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ConfirmDialog, refusalLine } from "../components/ConfirmDialog";
import { ConnectionProvider } from "../state/connection";
import { TransportError } from "../transport/types";
import { ADDRESS, FakeTransport, claimRecord, device, mergeAck, requestSummary } from "./fake-transport";

const HEAD = "a".repeat(40);

function seeded(): FakeTransport {
  const fake = new FakeTransport();
  fake.seedRequests([requestSummary("R-1"), mergeAck("R-2")], claimRecord({ generation: 3, holder: "ann@host" }));
  return fake;
}

interface Handlers {
  onActed: (acted: RequestActed) => void;
  onReplayed: (acted: RequestActed) => void;
  onSubjectMoved: () => void;
  onClose: () => void;
}

function mount(fake: FakeTransport, props: { kind: "merge" | "answer"; request: RequestSummary; text?: string }): Handlers {
  const handlers: Handlers = {
    onActed: vi.fn(),
    onReplayed: vi.fn(),
    onSubjectMoved: vi.fn(),
    onClose: vi.fn(),
  };
  render(
    <ConnectionProvider
      transport={fake}
      server={{ address: ADDRESS, device: device("D-1", "MacBook") }}
      onForgotten={() => {}}
      onPairAgain={() => {}}
    >
      <ConfirmDialog kind={props.kind} request={props.request} text={props.text ?? ""} {...handlers} />
    </ConnectionProvider>,
  );
  return handlers;
}

const dialog = () => within(screen.getByRole("dialog"));

describe("the confirm dialog as a component", () => {
  it("asks for a merge code with the request and its revision, and no hash, and shows what it binds", async () => {
    const fake = seeded();
    mount(fake, { kind: "merge", request: mergeAck("R-2", { head_sha: HEAD }) });
    await dialog().findByText("spine/s1");
    expect(fake.confirmArgs).toHaveLength(1);
    expect(fake.confirmArgs[0]?.subject).toEqual({ kind: "request", request: "R-2", revision: 1, verb: "ack" });
    expect(dialog().getByText("main")).toBeTruthy();
    expect(dialog().getByText(HEAD)).toBeTruthy();
    expect(dialog().getByText("Takes the claim from ann@host")).toBeTruthy();
    expect(fake.count("ack")).toBe(0);
  });

  it("asks for an answer code with the SHA-256 of the text, and shows the exact text", async () => {
    const fake = seeded();
    mount(fake, { kind: "answer", request: requestSummary("R-1"), text: "abc" });
    await dialog().findByText("abc");
    expect(fake.confirmArgs[0]?.subject).toEqual({
      kind: "request",
      request: "R-1",
      revision: 1,
      verb: "answer",
      text_sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    });
  });

  it("sends the act with the code it holds and a new command ID on the second click, then closes", async () => {
    const fake = seeded();
    const handlers = mount(fake, { kind: "merge", request: mergeAck("R-2") });
    await dialog().findByText("spine/s1");
    await userEvent.setup().click(dialog().getByRole("button", { name: "Merge" }));

    await waitFor(() => expect(handlers.onActed).toHaveBeenCalledTimes(1));
    expect(fake.actArgs[0]?.verb).toBe("ack");
    expect(fake.actArgs[0]?.args).toMatchObject({ request: "R-2", rev: 1, code: `pfc1-${"1".padStart(32, "0")}` });
    expect(fake.actArgs[0]?.args.command_id).not.toBe(fake.confirmArgs[0]?.command_id);
    expect(handlers.onClose).toHaveBeenCalled();
  });

  it("sends nothing on Cancel and on Escape", async () => {
    const fake = seeded();
    const user = userEvent.setup();
    const handlers = mount(fake, { kind: "merge", request: mergeAck("R-2") });
    await dialog().findByText("spine/s1");
    await user.click(dialog().getByRole("button", { name: "Cancel" }));
    await user.keyboard("{Escape}");
    expect(handlers.onClose).toHaveBeenCalledTimes(2);
    expect(fake.count("ack")).toBe(0);
  });

  it("subject_moved says the request changed and tells the page once", async () => {
    const fake = seeded();
    fake.fail("ack", "refused", "the server's text", 1, "subject_moved");
    const handlers = mount(fake, { kind: "merge", request: mergeAck("R-2") });
    await dialog().findByText("spine/s1");
    await userEvent.setup().click(dialog().getByRole("button", { name: "Merge" }));
    expect(await dialog().findByText("The request changed since the code was issued. Read it again and confirm.")).toBeTruthy();
    expect(handlers.onSubjectMoved).toHaveBeenCalledTimes(1);
  });

  it("unreachable on the second click keeps the code and the command ID: Retry resends both", async () => {
    const fake = seeded();
    fake.fail("ack", "unreachable", "no answer", 1);
    mount(fake, { kind: "merge", request: mergeAck("R-2") });
    await dialog().findByText("spine/s1");
    const user = userEvent.setup();
    await user.click(dialog().getByRole("button", { name: "Merge" }));
    await user.click(await dialog().findByRole("button", { name: "Retry" }));
    await waitFor(() => expect(fake.actArgs).toHaveLength(2));
    expect(fake.actArgs[1]?.args.code).toBe(fake.actArgs[0]?.args.code);
    expect(fake.actArgs[1]?.args.command_id).toBe(fake.actArgs[0]?.args.command_id);
    expect(fake.count("confirmCode")).toBe(1);
  });

  it("holds the code in no DOM text", async () => {
    const fake = seeded();
    mount(fake, { kind: "merge", request: mergeAck("R-2") });
    await dialog().findByText("spine/s1");
    expect(document.body.innerHTML).not.toContain("pfc1-");
  });
});

describe("refusalLine", () => {
  it.each([
    ["code_expired", "The confirm code expired. Confirm again."],
    ["code_used", "This confirm code cannot be used (code_used). Confirm again."],
    ["subject_moved", "The request changed since the code was issued. Read it again and confirm."],
    ["claim_moved", "The claim moved to bob. Confirm again."],
  ])("%s gives its own line", (word, line) => {
    expect(refusalLine(new TransportError("refused", "The claim moved to bob.", word))).toBe(line);
  });

  it("shows the server's text for a refusal with no word", () => {
    expect(refusalLine(new TransportError("refused", "the checks are red"))).toBe("the checks are red");
  });
});
