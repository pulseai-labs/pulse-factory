// AC-1 (r2.s11.w5): the Needs you cards read the decision block (12a to 12d). A card shows the kind label, the
// controller's summary as stored, who asked, the attempt and the IDs; one button for each option with who wrote
// it and the recommendation; the text field of the form the request takes; and the answered row names what was
// done. The summaries are the controller's (`summary_of`): the app never builds or rewrites one. Block strings
// are data and render as text only (G2).
import type { Decision, DecisionOption, RequestSummary } from "@pulse-factory/api-client";
import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { AnsweredRow } from "../needs-you/AnsweredRow";
import { RequestCard } from "../needs-you/cards";
import { cutOptionsBlock, readable } from "../needs-you/decision";
import { answerOption, approveOption, cardEnv, decisionBlock, detailOf, resumeOption } from "./decision-fixtures";
import { mergeAck, requestSummary } from "./fake-transport";

/** Draw one request as its card, from a block and a body. */
function draw(
  request: RequestSummary,
  decision: Decision | undefined,
  options: { body?: unknown; compact?: boolean; env?: Parameters<typeof cardEnv>[0] } = {},
) {
  const env = cardEnv({
    detail: detailOf(request, decision, options.body ?? {}),
    compact: options.compact ?? false,
    ...options.env,
  });
  render(<RequestCard request={request} env={env} />);
  return { card: within(screen.getByRole("article", { name: `Request ${request.id}` })), env };
}

type Card = ReturnType<typeof within>;
const buttons = (card: Card): HTMLElement[] => card.queryAllByRole("button").filter((b: HTMLElement) => b.hasAttribute("data-option"));
const optionIds = (card: Card) => buttons(card).map((b: HTMLElement) => b.getAttribute("data-option"));

const STOP_TEXT = "Which database?\nOptions:\ns1. Use SQLite\ns2. Use Postgres";

function stop(id = "R-12", patch: Partial<RequestSummary> = {}): RequestSummary {
  return requestSummary(id, { kind: "stop", unit: "u1", stage: "implement", ...patch });
}

describe("the kind label", () => {
  const KINDS: Array<[kind: string, label: string]> = [
    ["stop", "QUESTION"],
    ["gaps", "GAPS"],
    ["red_gate", "RED GATE"],
    ["park", "PARKED"],
    ["stall", "STALLED"],
    ["seat_lost", "SEAT LOST"],
    ["merge_failed", "MERGE FAILED"],
    ["merge_ack", "MERGE APPROVAL"],
  ];

  it.each(KINDS)("%s leads its card with %s", (kind, label) => {
    const request = kind === "merge_ack" ? mergeAck("R-3") : requestSummary("R-3", { kind, unit: "u1" });
    const { card } = draw(request, decisionBlock({ cause: kind === "merge_ack" ? "merge_waits_ack" : "park_file", options: [] }));
    expect(card.getByTestId("kind-label").textContent).toBe(label);
  });

  it("shows another kind's stored text in upper case, with the PARKED look", () => {
    const { card } = draw(requestSummary("R-3", { kind: "odd_kind", unit: "u1" }), decisionBlock({ cause: "odd_cause" }));
    const tag = card.getByTestId("kind-label");
    expect(tag.textContent).toBe("ODD_KIND");
    expect(tag.className).toBe(screen.getByRole("article").querySelector(".ny-tag-violet")?.className);
  });
});

describe("the summary is the controller's, as stored", () => {
  // The sentences of Approach 2's table, with the IDs filled in as the controller writes them.
  const SENTENCES = [
    "Unit u1 stopped at stage implement and wrote a stop file.",
    "Unit u1 parked at stage implement because the seat wrote a park file.",
    "The seat of unit u1 ended its turn at stage implement with no done signal (attempt 2).",
    "The seat of unit u1 stalled at stage implement (attempt 2) after its one relaunch.",
    "The agent refused to launch stage implement of unit u1 (attempt 1).",
    "Launching stage implement of unit u1 failed (attempt 1).",
    "Stage review of unit u1 sent the unit to fix 3 times, which is its cap, so the unit parked.",
    "Stage review of unit u1 reached its cap on the way to fix, so the unit parked.",
    "A command hook of stage implement of unit u1 failed.",
    "The answer hook of stage implement of unit u1 failed, so the unit parked again.",
    "A pinned copy changed after the run armed, so unit u1 parked before stage implement.",
    "The push of the base branch before stage implement of unit u1 failed.",
    "The default branch moved during a push of unit u1 at stage merge.",
    "A path or a condition of stage implement of unit u1 cannot render.",
    "The decisions file of unit u1 is over its size bound.",
    "Unit u1 is ready to merge, but no PR is recorded for it.",
    "the PR o/r#9 of unit u1 conflicts with its base.",
    "The checks of the PR o/r#9 of unit u1 are red.",
    "The forge refused to merge the PR o/r#9 of unit u1.",
    "The merge of the PR o/r#9 of unit u1 did not land.",
    "The seat of unit u1 is gone: its process ended at stage implement (attempt 1).",
    "The host of the seat of unit u1 did not answer at stage implement (attempt 1).",
    "the PR o/r#9 of unit u1 is ready to merge. It waits for your ack of head aaaaaaaaaaaa.",
    "the PR of unit u1 is ready to merge. It waits for your ack.",
  ];

  it.each(SENTENCES)("reads %s back unchanged", (sentence) => {
    const merge = sentence.includes("ready to merge. It waits");
    const request = merge ? mergeAck("R-4") : requestSummary("R-4", { kind: "park", unit: "u1" });
    const { card } = draw(request, decisionBlock({ cause: merge ? "merge_waits_ack" : "park_file", summary: sentence }));
    expect(card.getByTestId("summary").textContent).toBe(sentence);
  });

  it("draws no placeholder tag", () => {
    draw(stop(), decisionBlock());
    expect(screen.queryByText("placeholder")).toBeNull();
  });
});

describe("the facts, the head and the IDs", () => {
  it("shows a question's rows: Asked by, Attempt, Run, with no Cause row (N3)", () => {
    const { card } = draw(stop(), decisionBlock({ attempt: 1 }), { body: { stop_text: "Which database?" } });
    const labels = [...document.querySelectorAll(".ny-fact dt")].map((n) => n.textContent);
    expect(labels).toEqual(["Asked by", "Attempt", "Run"]);
    const values = [...document.querySelectorAll(".ny-fact dd")].map((n) => n.textContent);
    expect(values).toEqual(["Implementer · u1 · R-12", "1", "s1 · pulse-factory"]);
    expect(card.queryByText("Cause")).toBeNull();
  });

  it("shows a seat's park file's rows in order: Cause, Asked by, Attempt, Stage", () => {
    draw(requestSummary("R-15", { kind: "gaps", unit: "u2", stage: "plan" }), decisionBlock({
      cause: "park_file",
      asked_by: { kind: "seat", role: "round_planner", unit: "u2" },
      refs: { project: "P-1", run: "R-1", unit: "u2", stage: "plan" },
    }));
    expect([...document.querySelectorAll(".ny-fact dt")].map((n) => n.textContent)).toEqual(["Cause", "Asked by", "Attempt", "Stage"]);
    expect([...document.querySelectorAll(".ny-fact dd")].map((n) => n.textContent)).toEqual(["park_file", "Round planner · u2 · R-15", "1", "plan"]);
  });

  it("shows a park the controller opened: Cause, Attempt, Parked by, About, Stage; `spine_close` reads Spine close", () => {
    draw(requestSummary("R-21", { kind: "stall", unit: "u3", stage: "build" }), decisionBlock({
      cause: "stalled",
      asked_by: { kind: "controller" },
      about: { role: "spine_close", unit: "u3" },
      attempt: 3,
    }));
    expect([...document.querySelectorAll(".ny-fact dt")].map((n) => n.textContent)).toEqual(["Cause", "Attempt", "Parked by", "About", "Stage"]);
    expect([...document.querySelectorAll(".ny-fact dd")].map((n) => n.textContent)).toEqual(["stalled", "3", "the controller", "Spine close · u3", "implement"]);
    expect(readable("spine_close")).toBe("Spine close");
    expect(readable("round-planner")).toBe("Round planner");
  });

  it("leaves out About when the block holds none, and shows an attempt of 3 as `3` with no `of`", () => {
    draw(requestSummary("R-21", { kind: "park", unit: "u3" }), decisionBlock({ cause: "outcome_cap", asked_by: { kind: "controller" }, attempt: 3 }));
    expect(screen.queryByText("About")).toBeNull();
    expect(screen.getByText("Attempt").nextElementSibling?.textContent).toBe("3");
    expect(document.body.textContent).not.toMatch(/\b3 of\b/);
  });

  it("shows no Attempt row for a stored 0 (a park at the merge)", () => {
    draw(requestSummary("R-25", { kind: "merge_failed", unit: "u1", stage: "merge" }), decisionBlock({
      cause: "merge_checks_red",
      asked_by: { kind: "controller" },
      attempt: 0,
      refs: { project: "P-1", run: "R-1", unit: "u1", stage: "merge", pr: "o/r#212" },
    }));
    expect(screen.queryByText("Attempt")).toBeNull();
    expect(screen.getByText("Parked by")).toBeTruthy();
  });

  it("folds the facts into one line in the dock", () => {
    draw(stop(), decisionBlock(), { compact: true, body: { stop_text: "Which database?" } });
    expect(document.querySelector(".ny-facts")).toBeNull();
    expect(screen.getByTestId("facts-line").textContent).toBe("Asked by Implementer · u1 · R-12 · Attempt 1 · Run s1 · pulse-factory");
  });

  it("heads the page card with the project, the time and the ID, and the dock card with `<R-n> · <age>` and `<spine> · <project>`", () => {
    const page = draw(stop(), decisionBlock());
    expect(page.card.getByText("· pulse-factory")).toBeTruthy();
    expect(page.card.getByText("R-12")).toBeTruthy();
    document.body.innerHTML = "";
    const dock = draw(stop(), decisionBlock(), { compact: true });
    expect(dock.card.getByText(/^R-12 · /)).toBeTruthy();
    expect(dock.card.getByText("· pulse-factory")).toBeTruthy();
    expect(dock.card.queryByRole("button", { name: "Open" })).toBeNull();
  });

  it("heads a run armed from the CLI (no project) with the spine and the run ID", () => {
    draw(stop(), decisionBlock({ refs: { run: "R-1", unit: "u1", stage: "implement" } }));
    expect(screen.getByText("· R-1")).toBeTruthy();
    expect(document.querySelector(".ny-idchip-label")?.textContent).toBe("run");
  });

  it("draws one chip for each ID that refs holds, in order, and only those", () => {
    draw(requestSummary("R-25", { kind: "merge_failed", unit: "u1", stage: "merge" }), decisionBlock({
      cause: "merge_error",
      asked_by: { kind: "controller" },
      refs: { project: "P-1", run: "R-1", stage: "merge", pr: "pulseai-labs/example#212" },
    }));
    const chips = [...document.querySelectorAll(".ny-idchip")].map((n) => n.textContent);
    expect(chips).toEqual(["projectpulse-factory", "runR-1", "stagemerge", "PR #212"]);
    expect(document.querySelector('.ny-idchip[title="pulseai-labs/example#212"]')).not.toBeNull();
  });
});

describe("the question", () => {
  const seatOptions = [answerOption("s1", "Use SQLite"), answerOption("s2", "Use Postgres")];

  it("shows `<Role> <unit> asks` and cuts the options block when the block holds the seat's options", () => {
    const { card } = draw(stop(), decisionBlock({ options: seatOptions }), { body: { stop_text: STOP_TEXT } });
    expect(card.getByText("Implementer u1 asks")).toBeTruthy();
    expect(card.getByTestId("stop-text").textContent).toBe("Which database?");
  });

  it("shows the whole text with options_error and with no option", () => {
    draw(stop("R-1"), decisionBlock({ options: [], options_error: "entry 2 has no label" }), { body: { stop_text: STOP_TEXT } });
    expect(screen.getByTestId("stop-text").textContent).toBe(STOP_TEXT);
    document.body.innerHTML = "";
    draw(stop("R-2"), decisionBlock({ options: [] }), { body: { stop_text: STOP_TEXT } });
    expect(screen.getByTestId("stop-text").textContent).toBe(STOP_TEXT);
  });

  it("cuts only before the last `Options:` line", () => {
    expect(cutOptionsBlock("a\nOptions:\nb\nOptions:\nc")).toBe("a\nOptions:\nb");
    expect(cutOptionsBlock("no block")).toBe("no block");
  });

  it("shows a red gate's reason on one line, with the whole line as its title", () => {
    const reason = "migrations/down: 0007 has no down step; rollback test failed (exit 1)";
    draw(requestSummary("R-18", { kind: "red_gate", unit: "u1", stage: "close", reason: `${reason}\nsecond line` }), decisionBlock({
      cause: "park_file",
      options: [],
    }));
    expect(screen.getByText("The gate’s reason")).toBeTruthy();
    const line = document.querySelector(".ny-gate-reason");
    expect(line?.getAttribute("title")).toBe(`${reason}\nsecond line`);
    expect(line?.textContent).toBe(reason);
  });

  it("shows a gaps request's gaps as today", () => {
    draw(requestSummary("R-15", { kind: "gaps", unit: "u2", stage: "plan" }), decisionBlock({ cause: "park_file" }), {
      body: { gaps: [{ section: "§2", question: "Which table owns agents?", severity: "blocking" }] },
    });
    expect(screen.getByText("Which table owns agents?")).toBeTruthy();
    expect(screen.getByText("blocking")).toBeTruthy();
  });
});

describe("the options", () => {
  it("draws one full-width button for each option, in the block's order, with the whole label", () => {
    const long = "Stack them: apply the coupon first, then the loyalty discount, and keep both lines on the receipt so the buyer sees each one";
    const { card } = draw(stop(), decisionBlock({ options: [answerOption("s1", long), answerOption("s2", "Replace it"), answerOption("s3", "Third"), answerOption("s4", "Fourth")] }));
    expect(optionIds(card)).toEqual(["s1", "s2", "s3", "s4"]);
    expect(buttons(card)[0]?.querySelector(".ny-opt-label")?.textContent).toBe(long);
  });

  const unit = "u1";
  const EFFECTS: Array<[name: string, option: DecisionOption, patch: Partial<Decision>, stage: string, line: string]> = [
    ["answer with no to", answerOption("s1", "A"), {}, "implement", "Sends an answer"],
    ["answer with to", answerOption("w1", "B", { by: "workflow", effect: { verb: "answer", text: "go", to: "correction" } }), {}, "implement", "Answers, then resumes at correction"],
    ["resume, outcome_cap", resumeOption("c1", "Send it to fix again", "fix"), { cause: "outcome_cap", asked_by: { kind: "controller" } }, "review", `Resumes ${unit} at fix; the outcome counters start again`],
    ["resume, no_done_signal", resumeOption("c1", "Resume"), { cause: "no_done_signal", asked_by: { kind: "controller" } }, "build", `Resumes ${unit} at build; the stage launches again`],
    ["resume, launch_failed", resumeOption("c1", "Resume"), { cause: "launch_failed", asked_by: { kind: "controller" } }, "build", `Resumes ${unit} at build; the stage launches again`],
    ["resume, launch_refused", resumeOption("c1", "Resume"), { cause: "launch_refused", asked_by: { kind: "controller" } }, "build", `Resumes ${unit} at build; the stage launches again`],
    ["resume at the merge stage", resumeOption("c1", "Resume"), { cause: "merge_checks_red", asked_by: { kind: "controller" } }, "merge", "Tries the merge again"],
    ["resume, any other cause", resumeOption("c1", "Resume"), { cause: "stalled", asked_by: { kind: "controller" } }, "build", `Resumes ${unit} at build`],
  ];

  it.each(EFFECTS)("words %s", (_name, option, patch, stage, line) => {
    const { card } = draw(requestSummary("R-9", { kind: "park", unit, stage }), decisionBlock({ options: [option], ...patch }));
    expect(buttons(card)[0]?.querySelector(".ny-opt-does")?.textContent).toBe(line);
  });

  it("draws the stage of an effect as a chip", () => {
    const { card } = draw(requestSummary("R-9", { kind: "park", unit, stage: "review" }), decisionBlock({ cause: "outcome_cap", asked_by: { kind: "controller" }, options: [resumeOption("c1", "Send it to fix again", "fix")] }));
    expect(buttons(card)[0]?.querySelector(".ny-stage")?.textContent).toBe("fix");
  });

  it("names the writer: the seat, the workflow, the controller, and a block with no `by`", () => {
    const options = [
      answerOption("s1", "From the seat", { by: "seat" }),
      answerOption("w1", "From the workflow", { by: "workflow" }),
      answerOption("c1", "From the controller", { by: "controller" }),
    ];
    const { card } = draw(stop(), decisionBlock({ options }));
    expect(buttons(card).map((b: HTMLElement) => b.querySelector(".ny-opt-by")?.textContent)).toEqual(["from Implementer u1", "from the workflow", "from the controller"]);
    document.body.innerHTML = "";
    // No `by`: a stop reads `seat`, any other cause reads `controller`.
    const noBy = (id: string): DecisionOption => {
      const { by: _by, ...rest } = answerOption(id, "No writer");
      return rest;
    };
    const a = draw(stop("R-20"), decisionBlock({ options: [noBy("s1")] }));
    expect(buttons(a.card)[0]?.querySelector(".ny-opt-by")?.textContent).toBe("from Implementer u1");
    document.body.innerHTML = "";
    const b = draw(requestSummary("R-21", { kind: "park", unit }), decisionBlock({ cause: "park_file", asked_by: { kind: "controller" }, options: [{ ...resumeOption("c1", "Resume"), by: undefined }] }));
    expect(buttons(b.card)[0]?.querySelector(".ny-opt-by")?.textContent).toBe("from the controller");
  });

  it("puts a workflow option's answer text in the button's title and accessible description, before the stored detail", () => {
    const option = answerOption("w1", "Let the Correction seat add the down step", {
      by: "workflow",
      detail: "The workflow wrote this option.",
      effect: { verb: "answer", text: "Add a down step to 0007 and run the rollback again.", to: "correction" },
    });
    const { card } = draw(requestSummary("R-18", { kind: "red_gate", unit }), decisionBlock({ cause: "park_file", options: [option] }));
    const button = buttons(card)[0] as HTMLElement;
    expect(button.getAttribute("title")).toBe("Sends: Add a down step to 0007 and run the rollback again.\nThe workflow wrote this option.");
    const described = document.getElementById(button.getAttribute("aria-describedby") ?? "");
    expect(described?.textContent).toBe(button.getAttribute("title"));
  });

  it("marks the recommended option with its source and reason, for a seat, a workflow and the controller, on one option only", () => {
    const seat = draw(stop("R-1"), decisionBlock({
      options: [answerOption("s1", "A"), answerOption("s2", "B")],
      recommended: { option: "s2", by: "seat", why: "The seat marked this option." },
    }));
    expect(document.querySelectorAll(".ny-opt-rec")).toHaveLength(1);
    expect(seat.card.getByText("Recommended by Implementer u1")).toBeTruthy();
    expect(seat.card.getByText("· The seat marked this option.")).toBeTruthy();
    expect(buttons(seat.card)[1]?.className).toContain("ny-opt-rec");
    document.body.innerHTML = "";
    const wf = draw(requestSummary("R-2", { kind: "red_gate", unit }), decisionBlock({
      cause: "park_file",
      options: [answerOption("w1", "A", { by: "workflow" }), answerOption("w2", "B", { by: "workflow" })],
      recommended: { option: "w1", by: "workflow", why: "The workflow marked this option." },
    }));
    expect(wf.card.getByText("Recommended by the workflow")).toBeTruthy();
    expect(document.querySelectorAll(".ny-opt-rec")).toHaveLength(1);
    document.body.innerHTML = "";
    const ctl = draw(requestSummary("R-3", { kind: "stall", unit }), decisionBlock({
      cause: "stalled",
      asked_by: { kind: "controller" },
      options: [resumeOption("c1", "Resume")],
      recommended: { option: "c1", by: "controller", why: "A stall is often transient: a new launch may go on." },
    }));
    expect(ctl.card.getByText("Recommended by the controller")).toBeTruthy();
    expect(ctl.card.getByText("· A stall is often transient: a new launch may go on.")).toBeTruthy();
  });

  it("never shows the recommended mark on a merge approval, whatever its block holds", () => {
    draw(mergeAck("R-19"), decisionBlock({
      cause: "merge_waits_ack",
      asked_by: { kind: "controller" },
      options: [approveOption()],
      recommended: { option: "approve", by: "controller", why: "Checks are green." },
    }));
    expect(document.querySelector(".ny-opt-rec")).toBeNull();
    expect(screen.queryByText(/Recommended by/)).toBeNull();
  });

  it("warns a seat's park file that offers a resume option, above its options", () => {
    const { card } = draw(requestSummary("R-23", { kind: "park", unit: "w3", stage: "review" }), decisionBlock({
      cause: "park_file",
      options: [resumeOption("c1", "Resume", undefined)],
      refs: { run: "R-1", unit: "w3", stage: "review" },
    }));
    expect(card.getByText("Remove the park file first. If it is still there when w3 resumes, the unit parks again.")).toBeTruthy();
    const warn = card.getByText(/Remove the park file first/);
    const opt = buttons(card)[0] as HTMLElement;
    expect(warn.compareDocumentPosition(opt) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("says the options could not be read, with the stored cause as the line's title", () => {
    const { card } = draw(stop(), decisionBlock({ options_error: "entry 2 has no label", options: [] }));
    const line = card.getByText("The seat wrote options that couldn’t be read, so none are shown. Answer in your own words.");
    expect(line.closest("[title]")?.getAttribute("title")).toBe("entry 2 has no label");
    expect(buttons(card)).toHaveLength(0);
    expect(card.getByLabelText("Another answer")).toBeTruthy();
  });

  it("draws no button for an answer option when takes_text is false, and still draws a resume option", () => {
    const { card } = draw(requestSummary("R-9", { kind: "park", unit }), decisionBlock({
      cause: "park_file",
      takes_text: false,
      options: [answerOption("s1", "Answer"), resumeOption("c1", "Resume")],
    }));
    expect(optionIds(card)).toEqual(["c1"]);
    expect(card.queryByRole("textbox")).toBeNull();
  });

  it("draws no answer option when the request does not list `answer`", () => {
    const { card } = draw(requestSummary("R-9", { kind: "stop", unit, verbs: ["clear"] }), decisionBlock({ options: [answerOption("s1", "Answer")] }));
    expect(buttons(card)).toHaveLength(0);
  });

  it("offers a stop that takes no text one Resume, with its warning and no writer", () => {
    const { card } = draw(stop("R-9", { stage: "implement" }), decisionBlock({ takes_text: false, options: [answerOption("s1", "A"), answerOption("s2", "B")] }), {
      body: { stop_text: STOP_TEXT },
    });
    expect(optionIds(card)).toEqual(["resume"]);
    expect(card.getByText("No answer can reach the seat: this stage declares no answer hook. Remove the stop file first. If it is still there when u1 resumes, the unit stops again.")).toBeTruthy();
    expect(buttons(card)[0]?.querySelector(".ny-opt-by")).toBeNull();
    expect(buttons(card)[0]?.querySelector(".ny-opt-does")?.textContent).toBe("Resumes u1 at implement");
    // The seat's own lines still show: nothing cuts them while no button draws them.
    expect(card.getByTestId("stop-text").textContent).toBe(STOP_TEXT);
    expect(card.queryByRole("textbox")).toBeNull();
  });
});

describe("the text field", () => {
  it("is a typed answer with Send when a seat asked: the frames' labels and placeholders", () => {
    const q = draw(stop(), decisionBlock({ options: [answerOption("s1", "A")] }));
    const field = q.card.getByLabelText("Another answer") as HTMLTextAreaElement;
    expect(field.placeholder).toBe("Type your own answer. The seat gets it word for word.");
    expect(q.card.getByRole("button", { name: "Send" })).toBeTruthy();
    document.body.innerHTML = "";
    const g = draw(requestSummary("R-15", { kind: "gaps", unit: "u2", stage: "plan" }), decisionBlock({ cause: "park_file" }));
    const gaps = g.card.getByLabelText("Your answer") as HTMLTextAreaElement;
    expect(gaps.placeholder).toBe("Answer each gap. The planner gets your text as written.");
    expect(g.card.getByRole("button", { name: "Send" })).toBeTruthy();
  });

  it("is a note with no Send when the controller asked, and takes the press of a resume option", () => {
    const { card } = draw(requestSummary("R-21", { kind: "stall", unit: "u3", stage: "build" }), decisionBlock({
      cause: "stalled",
      asked_by: { kind: "controller" },
      options: [resumeOption("c1", "Resume")],
    }));
    const field = card.getByLabelText("Note for the next seat") as HTMLTextAreaElement;
    expect(field.placeholder).toBe("Optional. It goes with the option you press.");
    expect(card.queryByRole("button", { name: "Send" })).toBeNull();
  });

  it("shows no field when takes_text is false", () => {
    const { card } = draw(requestSummary("R-20", { kind: "park", unit: "u4", stage: "review" }), decisionBlock({
      cause: "outcome_cap",
      asked_by: { kind: "controller" },
      takes_text: false,
      options: [resumeOption("c1", "Send it to fix again", "fix"), resumeOption("c2", "Go on here", "review")],
    }));
    expect(card.queryByRole("textbox")).toBeNull();
    expect(optionIds(card)).toEqual(["c1", "c2"]);
  });

  it("shows a seat's park file with answer options the typed-answer form (Another answer, Send), not the note (N2)", () => {
    const { card } = draw(requestSummary("R-22", { kind: "park", unit: "w1", stage: "review" }), decisionBlock({
      cause: "park_file",
      options: [answerOption("w1", "The schema is approved: go on", { by: "workflow", effect: { verb: "answer", text: "go", to: "build" } }), answerOption("w2", "Hold", { by: "workflow" })],
    }));
    expect(card.getByLabelText("Another answer")).toBeTruthy();
    expect(card.getByRole("button", { name: "Send" })).toBeTruthy();
    expect(card.queryByLabelText("Note for the next seat")).toBeNull();
  });
});

describe("the merge card", () => {
  it("shows the summary, the chips, From and Into, Holding up and the later-release controls", () => {
    const request = mergeAck("R-19", { blocks: ["u2"] });
    const { card } = draw(request, decisionBlock({
      cause: "merge_waits_ack",
      summary: "the PR pulseai-labs/example#9 of unit u1 is ready to merge. It waits for your ack of head aaaaaaaaaaaa.",
      asked_by: { kind: "controller" },
      options: [approveOption()],
      refs: { project: "P-1", run: "R-1", stage: "merge", pr: "pulseai-labs/example#9" },
    }), { body: { head_branch: "spine/s1", pr: "https://github.com/pulseai-labs/example/pull/9", base: "main", head_sha: "a".repeat(40) } });
    expect(card.getByTestId("summary").textContent).toContain("It waits for your ack of head aaaaaaaaaaaa.");
    expect(card.getByText("PR #9")).toBeTruthy();
    expect(card.getByTestId("merge-from").textContent).toBe("spine/s1");
    expect(card.getByTestId("merge-into").textContent).toBe("main");
    expect(card.getByText("Holding up")).toBeTruthy();
    expect(card.getByText("Approve the merge")).toBeTruthy();
    expect(card.getByText("Here, on your phone, or from the CLI")).toBeTruthy();
    expect(card.getByRole("button", { name: "Hold" })).toBeTruthy();
    expect(card.getByRole("button", { name: "Approve merge" })).toBeTruthy();
    for (const label of ["Review the PR", "Open on GitHub"]) {
      const later = card.getByText(label).closest("[data-not-built]");
      expect(later?.getAttribute("aria-disabled")).toBe("true");
      expect(later?.textContent).toContain("Later release");
    }
    expect(card.queryByText(/waits for the demo walk/i)).toBeNull();
  });
});

describe("a request with no block, and a card before its body", () => {
  it("draws today's card for a request with no block", () => {
    const request = stop("R-7");
    draw(request, undefined, { body: { stop_text: "Which database?" } });
    expect(screen.getByLabelText("Your answer")).toBeTruthy();
    expect(screen.getByText("Which database?")).toBeTruthy();
    expect(screen.queryByTestId("kind-label")).toBeNull();
    expect(document.querySelector(".ny-facts dt")?.textContent).toBe("Blocks");
  });

  it("draws its head and the row's summary and no act before the body arrives", () => {
    const request = stop("R-7", { summary: "Unit u1 stopped at stage implement and wrote a stop file.", cause: "stop_file" });
    render(<RequestCard request={request} env={cardEnv({ detail: undefined })} />);
    const card = within(screen.getByRole("article", { name: "Request R-7" }));
    expect(card.getByTestId("kind-label").textContent).toBe("QUESTION");
    expect(card.getByTestId("summary").textContent).toBe("Unit u1 stopped at stage implement and wrote a stop file.");
    expect(card.queryAllByRole("button")).toHaveLength(0);
    expect(card.queryByRole("textbox")).toBeNull();
  });

  it("keeps today's error line with Refresh when the body failed", () => {
    render(<RequestCard request={stop("R-7")} env={cardEnv({ detail: undefined, bodyError: "show failed" })} />);
    expect(screen.getByText("show failed")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Refresh" })).toBeTruthy();
  });
});

describe("the answered rows", () => {
  function row(
    request: RequestSummary,
    decision: Decision,
    answer: unknown,
    extra: Partial<Parameters<typeof AnsweredRow>[0]> = {},
    body: unknown = {},
  ) {
    const answered = { ...request, status: "answered", answered_at: new Date().toISOString(), verbs: [] };
    const detail = detailOf(answered, decision, body, answer);
    render(
      <ul>
        <AnsweredRow
          request={answered}
          body={detail}
          detail={detail}
          done={undefined}
          notice={undefined}
          spine="s1"
          claim={null}
          device={{ id: "D-1", name: "MacBook" }}
          projects={null}
          {...extra}
        />
      </ul>,
    );
    return within(screen.getByRole("listitem", { name: `Answered ${request.id}` }));
  }

  const options = [answerOption("s1", "Not case-sensitive: store codes in upper case.")];

  it("names a pressed option by its label, read from the block by the option's ID", () => {
    const r = row(stop("R-11"), decisionBlock({ options }), { verb: "answer", text: "stored text differs", option: { id: "s1", by: "seat", recommended: false } }, {}, { stop_text: "Should coupon codes be case-sensitive?\nOptions:\ns1. Not case-sensitive" });
    expect(r.getByTestId("kind-label").textContent).toBe("QUESTION");
    expect(r.getByText("Pressed Not case-sensitive: store codes in upper case.")).toBeTruthy();
    expect(r.getByText("Should coupon codes be case-sensitive?")).toBeTruthy();
    expect(r.getByText("R-11")).toBeTruthy();
  });

  it("names the stored text when the block no longer holds the option", () => {
    const r = row(stop("R-11"), decisionBlock({ options: [] }), { verb: "answer", text: "Use Postgres", option: { id: "s9", by: "seat", recommended: false } });
    expect(r.getByText("Pressed Use Postgres")).toBeTruthy();
  });

  it("names a typed answer on a request a seat asked", () => {
    const r = row(requestSummary("R-9", { kind: "gaps", unit: "u2" }), decisionBlock({ cause: "park_file" }), { verb: "answer", text: "Start from main after S-1 merges." });
    expect(r.getByText("Typed Start from main after S-1 merges.")).toBeTruthy();
  });

  it("names a resume by its unit and the answer's `to_stage`, not the stage the request held", () => {
    const stall = requestSummary("R-10", { kind: "stall", unit: "u3", stage: "build" });
    const block = decisionBlock({ cause: "stalled", asked_by: { kind: "controller" }, options: [resumeOption("c1", "Resume")] });
    const a = row(stall, block, { verb: "clear", to_stage: "fix", option: { id: "c1", by: "controller", recommended: true } });
    expect(a.getByText("Resumed u3 at fix")).toBeTruthy();
    document.body.innerHTML = "";
    const b = row(stall, block, { verb: "clear" });
    expect(b.getByText("Resumed u3 at build")).toBeTruthy();
  });

  it("names a note on a request the controller asked", () => {
    const stall = requestSummary("R-10", { kind: "stall", unit: "u3", stage: "build" });
    const r = row(stall, decisionBlock({ cause: "stalled", asked_by: { kind: "controller" }, options: [resumeOption("c1", "Resume")] }), { verb: "answer", text: "restarted the database", to_stage: "build" });
    expect(r.getByText("Resumed u3 at build · note: restarted the database")).toBeTruthy();
  });

  it("names a merge and a withdrawn request, as before", () => {
    const merged = row(mergeAck("R-19"), decisionBlock({ cause: "merge_waits_ack", asked_by: { kind: "controller" }, options: [approveOption()] }), { verb: "ack", head_sha: "a".repeat(40) });
    expect(merged.getByText("Merge approved")).toBeTruthy();
    expect(merged.getByText(`Merged at ${"a".repeat(7)}`)).toBeTruthy();
    document.body.innerHTML = "";
    const withdrawn = requestSummary("R-5", { kind: "stop", unit: "u1" });
    render(
      <ul>
        <AnsweredRow
          request={{ ...withdrawn, status: "withdrawn", answered_at: new Date().toISOString(), verbs: [] }}
          body={undefined}
          detail={undefined}
          done={undefined}
          notice={undefined}
          spine={null}
          claim={null}
          device={{ id: "D-1", name: "MacBook" }}
        />
      </ul>,
    );
    expect(screen.getByText("Withdrawn")).toBeTruthy();
  });

  it("shows the summary as the question of every other block card", () => {
    const stall = requestSummary("R-10", { kind: "stall", unit: "u3", stage: "build" });
    const r = row(stall, decisionBlock({ cause: "stalled", asked_by: { kind: "controller" }, summary: "The seat of unit u3 stalled at stage build (attempt 2) after its one relaunch." }), { verb: "clear" });
    expect(r.getByText("The seat of unit u3 stalled at stage build (attempt 2) after its one relaunch.")).toBeTruthy();
  });

  it("reads a row with no block as it did", () => {
    const old = { ...requestSummary("R-6", { kind: "stop", unit: "u1" }), status: "answered", answered_at: new Date().toISOString(), verbs: [] };
    const detail = detailOf(old, undefined, { stop_text: "Old question?" }, { verb: "answer", text: "old answer" });
    render(
      <ul>
        <AnsweredRow request={old} body={detail} detail={detail} done={undefined} notice={undefined} spine="s1" claim={null} device={{ id: "D-1", name: "MacBook" }} />
      </ul>,
    );
    expect(screen.queryByTestId("kind-label")).toBeNull();
    expect(screen.getByText("old answer")).toBeTruthy();
  });
});

describe("a block string is data", () => {
  it("renders markup in a summary, a label, a reason, a stop text and a note as text", () => {
    const evil = "<img src=x onerror=alert(1)><b>bold</b>";
    draw(stop("R-30"), decisionBlock({
      summary: evil,
      options: [answerOption("s1", evil, { detail: evil })],
      recommended: { option: "s1", by: "seat", why: evil },
      options_error: undefined,
    }), { body: { stop_text: evil } });
    expect(document.querySelector("img")).toBeNull();
    expect(document.querySelector("article b")).toBeNull();
    expect(screen.getByTestId("summary").textContent).toBe(evil);
    expect(screen.getByTestId("stop-text").textContent).toBe(evil);
  });
});
