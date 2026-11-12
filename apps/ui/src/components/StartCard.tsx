// The start card (r1.s4, Approach 8): a proposal that the Orchestrator chat posted, shown with the facts of
// the plan, and the two steps that arm it. **Confirm start** asks the server for a one-use code for this
// proposal and opens a dialog. **Arm the run** spends the code. The code lives in this component's state
// only: it is never shown, stored, logged or put in a URL. The card learns the outcome from the
// `start_result` item on the chat stream the shell already holds, so it has no timer and no stream of its
// own, and the expiry is a static time.
import type { ChatItemBody, ConfirmIssued } from "@pulse-factory/api-client";
import { useState } from "react";
import { asTransportError, useConnection } from "../state/connection";
import { timeText } from "./chat-format";
import { useCommandId } from "./command-id";
import { Button } from "../ui";

type Card = Extract<ChatItemBody, { kind: "start_card" }>;
type Result = Extract<ChatItemBody, { kind: "start_result" }>;

/** What a held code lets the dialog show. The code is kept for the arm call and never rendered. */
interface Held {
  code: string;
  expiresAt: string | null;
  device: string;
}

/** One fact of the plan. A fact with no line is left out. */
function Fact(props: { name: string; lines: string[] }) {
  if (props.lines.length === 0) return null;
  return (
    <div role="group" aria-label={props.name} className="start-fact">
      <span className="muted">{props.name}</span>
      {props.lines.map((line, i) => (
        <p key={i} className="entry-text">
          {line}
        </p>
      ))}
    </div>
  );
}

function line(value: string | number | null | undefined): string[] {
  return value === null || value === undefined || value === "" ? [] : [String(value)];
}

function outcomeText(result: Result): string {
  switch (result.result) {
    case "armed":
      return `Armed: run ${result.run ?? "unknown"}${result.device ? ` from ${result.device}` : ""}`;
    case "withdrawn":
      return `Withdrawn: ${result.reason ?? "the proposal was withdrawn"}`;
    case "failed":
      return `Failed: ${result.reason ?? "the run could not start"}`;
  }
}

export function StartCard(props: { card: Card; result: Result | null; readOnly: boolean }) {
  const { card, result, readOnly } = props;
  const { call } = useConnection();
  const confirmId = useCommandId();
  const armId = useCommandId();
  const [held, setHeld] = useState<Held | null>(null);
  const [busy, setBusy] = useState(false);
  // The arm went through: the card now waits for its `start_result` item.
  const [sent, setSent] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const s = card.summary;

  async function confirm() {
    if (busy) return;
    setBusy(true);
    setProblem(null);
    const id = confirmId.idFor("confirm");
    try {
      const out: ConfirmIssued = await call((t) =>
        t.confirmCode({ subject: { kind: "arm", proposal: card.proposal, plan_sha256: card.plan_sha256 }, command_id: id }),
      );
      confirmId.clear();
      if (out.code === null) {
        // A replay of an answer this app never received: the server cannot show the code again.
        setProblem("The code was issued before, and the server cannot show it again. Confirm the start again for a new one.");
      } else {
        setHeld({ code: out.code, expiresAt: out.expires_at, device: out.device });
      }
    } catch (e) {
      const err = asTransportError(e);
      if (err.kind !== "unreachable") confirmId.clear();
      setProblem(err.message);
    } finally {
      setBusy(false);
    }
  }

  async function arm() {
    if (busy || held === null) return;
    const code = held.code;
    setBusy(true);
    setProblem(null);
    const id = armId.idFor(`arm:${code}`);
    try {
      await call((t) => t.arm({ proposal: card.proposal, plan_sha256: card.plan_sha256, code, command_id: id }));
      armId.clear();
      setHeld(null);
      setSent(true);
    } catch (e) {
      const err = asTransportError(e);
      if (err.kind === "unreachable") {
        // The code stays for a retry, and the retry reuses the command ID.
        setProblem(err.message);
      } else {
        // Any refusal spends or kills the code: the next Confirm start asks for a new one.
        armId.clear();
        setHeld(null);
        setProblem(err.message);
      }
    } finally {
      setBusy(false);
    }
  }

  function cancel() {
    armId.clear();
    setHeld(null);
    setProblem(null);
  }

  const roles = s.roles.map((r) => `${r.name}: ${r.runtime}, ${r.model}`);
  const caps = [
    `${s.caps.model_sessions} model sessions`,
    `${s.caps.cold_builds} cold builds`,
    ...s.caps.seeds.map((x) => `seed ${x.id}: at most ${x.max}`),
    `relaunch: ${s.caps.relaunch}`,
    ...s.caps.loops.map((x) => `loop ${x.track} ${x.from} to ${x.to}: at most ${x.max}`),
  ];
  const params = Object.entries(card.params).map(([k, v]) => `${k}=${v}`);
  const forge = s.forge_repo === "" ? [] : [s.default_branch === "" ? s.forge_repo : `${s.forge_repo} (${s.default_branch})`];

  return (
    <article className="entry entry-card" aria-label="Start workflow" data-testid="start-card" data-proposal={card.proposal}>
      <div className="tool-head">
        <strong>{`Start ${s.spine}`}</strong>
        <span className="muted">needs your confirmation</span>
      </div>
      <Fact name="Spine" lines={line(s.spine)} />
      <Fact name="Project" lines={line(card.project_name)} />
      <Fact name="Items" lines={line(s.items)} />
      <Fact name="Workflow" lines={line(`${s.workflow.name} v${s.workflow.version}`)} />
      <Fact name="Roles" lines={roles} />
      <Fact name="Caps" lines={caps} />
      <Fact name="Plugins" lines={s.plugins.map((p) => `${p.name} ${p.version}`)} />
      <Fact name="Most launches" lines={line(s.most_launches)} />
      <Fact name="Merges" lines={s.merges.map((m) => `${m.track} into ${m.base}: ${m.sentence}`)} />
      <Fact name="Base pushes" lines={s.base_pushes.map((b) => `${b.track} ${b.stage}: ${b.dir} on ${b.branch}`)} />
      <Fact name="Forge" lines={forge} />
      <Fact name="Paper env" lines={line(s.paper)} />
      <Fact name="Ledger" lines={line(card.ledger)} />
      <Fact name="Parameters" lines={params} />

      {result !== null ? (
        <p role="status" data-testid="start-outcome">
          <strong>{outcomeText(result)}</strong>
        </p>
      ) : sent ? (
        <p role="status" data-testid="start-outcome">
          <strong>Arming…</strong>
        </p>
      ) : null}

      {result === null && !readOnly ? (
        <div className="card-actions">
          <Button
            variant="primary"
            data-testid="start-confirm"
            disabled={busy || sent || held !== null}
            onClick={() => void confirm()}
          >
            Confirm start
          </Button>
        </div>
      ) : null}

      {held !== null && result === null ? (
        <div role="dialog" aria-label={`Confirm the start of ${s.spine}`} data-testid="start-dialog" className="start-dialog">
          <p className="entry-text">{`Arm a run of ${s.spine} in ${card.project_name}.`}</p>
          <p className="muted">
            {[
              `workflow ${s.workflow.name} v${s.workflow.version}`,
              ...(s.items === null ? [] : [`${s.items} items`]),
              ...(s.most_launches === null ? [] : [`at most ${s.most_launches} launches`]),
              `${s.caps.model_sessions} model sessions`,
            ].join(", ")}
          </p>
          <p className="muted">{`Confirmed by device ${held.device}.`}</p>
          {held.expiresAt !== null ? <p className="muted">{`This confirmation expires at ${timeText(held.expiresAt)}.`}</p> : null}
          <div className="card-actions">
            <Button data-testid="start-arm" disabled={busy} onClick={() => void arm()}>
              Arm the run
            </Button>
            <Button disabled={busy} onClick={cancel}>
              Cancel
            </Button>
          </div>
          {problem ? (
            <p className="problem-title" role="alert">
              {problem}
            </p>
          ) : null}
        </div>
      ) : problem ? (
        <p className="problem-title" role="alert">
          {problem}
        </p>
      ) : null}
    </article>
  );
}
