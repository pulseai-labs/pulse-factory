// The start card (11c): a proposal that the Orchestrator chat posted, with the facts of the plan, and one
// button. **Start workflow** is one press (G1): it asks the server for a one-use code for this proposal and
// this plan, and at once arms the run with it. The code lives in this component's state only: it is never
// shown, stored, logged or put in a URL. The card learns the outcome from the `start_result` item on the
// chat stream the shell already holds, so it has no timer and no stream of its own.
import type { ChatItemBody } from "@pulse-factory/api-client";
import { useState } from "react";
import { asTransportError, useConnection } from "../state/connection";
import { Button, Chip, Eyebrow, Icon, NotBuilt, modelName } from "../ui";
import { timeText } from "./chat-format";
import { useCommandId } from "./command-id";

type Card = Extract<ChatItemBody, { kind: "start_card" }>;
type Result = Extract<ChatItemBody, { kind: "start_result" }>;

/** One tile of the Limits group. */
function Tile(props: { value: string | number; label: string; name?: string }) {
  return (
    <div className="limit-tile" {...(props.name ? { role: "group", "aria-label": props.name } : {})}>
      <span className="limit-value">{props.value}</span>
      <span className="limit-label">{props.label}</span>
    </div>
  );
}

function Section(props: { name: string; aside?: string; children: React.ReactNode }) {
  return (
    <div className="card-section" role="group" aria-label={props.name}>
      <div className="section-head">
        <span className="section-title">{props.name}</span>
        {props.aside ? <span className="section-aside">{props.aside}</span> : null}
      </div>
      {props.children}
    </div>
  );
}

export function StartCard(props: {
  seq: number;
  card: Card;
  /** When the proposal was posted (the card item's `created_at`). */
  proposedAt: string;
  result: Result | null;
  /** When the result was posted (the `start_result` item's `created_at`). */
  resultAt: string | null;
  readOnly: boolean;
}) {
  const { seq, card, proposedAt, result, resultAt, readOnly } = props;
  const { call, server } = useConnection();
  const confirmId = useCommandId();
  const armId = useCommandId();
  // The code of the press that is under way. It is kept after `unreachable` on `arm`, for the next press.
  const [code, setCode] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // The arm went through: the card now waits for its `start_result` item.
  const [sent, setSent] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const s = card.summary;

  async function start() {
    if (busy || sent) return;
    setBusy(true);
    setProblem(null);
    let held = code;
    let stage: "confirm" | "arm" = "confirm";
    try {
      if (held === null) {
        const id = confirmId.idFor("confirm");
        const out = await call((t) =>
          t.confirmCode({ subject: { kind: "arm", proposal: card.proposal, plan_sha256: card.plan_sha256 }, command_id: id }),
        );
        confirmId.clear();
        if (out.code === null) {
          // A replay of an answer this app never received: the server cannot show the code again.
          setProblem("The code was issued before, and the server cannot show it again. Press Start workflow again for a new one.");
          return;
        }
        held = out.code;
        setCode(held);
      }
      stage = "arm";
      const spent = held;
      const id = armId.idFor(`arm:${spent}`);
      await call((t) => t.arm({ proposal: card.proposal, plan_sha256: card.plan_sha256, code: spent, command_id: id }));
      armId.clear();
      setCode(null);
      setSent(true);
    } catch (e) {
      const err = asTransportError(e);
      if (stage === "confirm") {
        if (err.kind !== "unreachable") confirmId.clear();
      } else if (err.kind !== "unreachable") {
        // Any refusal spends or kills the code: the next press asks for a new one.
        armId.clear();
        setCode(null);
      }
      // After `unreachable` on `arm`, the code and the command ID stay: the next press retries `arm` with them.
      setProblem(err.message);
    } finally {
      setBusy(false);
    }
  }

  const others = [
    ...s.caps.seeds.map((x) => `seed ${x.id}: at most ${x.max}`),
    `relaunch: ${s.caps.relaunch}`,
    ...s.caps.loops.map((x) => `loop ${x.track} ${x.from} to ${x.to}: at most ${x.max}`),
  ].join(" · ");

  const device = result?.device ? (result.device === server.device.id ? server.device.name : result.device) : "";
  const at = resultAt === null ? "" : timeText(resultAt);

  return (
    <article className="entry start-card" aria-label="Start workflow" data-testid="start-card" data-proposal={card.proposal} data-seq={seq}>
      <div className="start-head">
        <div className="start-eyebrow">
          <Icon name="activity" size={14} />
          <Eyebrow>Start a workflow</Eyebrow>
          <span className="start-proposed">{`proposed ${timeText(proposedAt)}`}</span>
        </div>
        <div className="start-title">
          <span className="start-spine">{s.spine}</span>
          <Chip>{card.project_name}</Chip>
          {s.paper ? (
            <Chip tone="mint" role="group" aria-label="Paper env" title="No model, no spend">
              Paper
            </Chip>
          ) : null}
        </div>

        <div className="limits" role="group" aria-label="Limits">
          <div className="limit-tiles">
            <Tile value={s.caps.model_sessions} label="Model sessions" />
            <Tile value={s.caps.cold_builds} label="Cold builds" />
            {s.most_launches === null ? null : <Tile value={s.most_launches} label="Most sessions" name="Most sessions" />}
          </div>
          <p className="limit-line">{others}</p>
          <p className="limit-note">The run stops and asks you when it reaches a limit.</p>
        </div>
        {s.merges.length === 0 ? null : (
          <div className="merge-line" role="group" aria-label="Merge line">
            <Icon name="git-branch" size={14} />
            <span>{s.merges.map((m) => m.sentence).join(" · ")}</span>
          </div>
        )}
      </div>

      {s.items === null ? null : (
        <Section name="Work items">
          <p className="section-body">{`${s.items} work ${s.items === 1 ? "item" : "items"}`}</p>
        </Section>
      )}

      <Section name="Who runs it">
        <ul className="role-list">
          {s.roles.map((r) => (
            <li key={r.name} className="role-row">
              <span className="role-name">{r.name}</span>
              <span className="role-model">{modelName(r.model)}</span>
              <Chip mono>{r.runtime}</Chip>
            </li>
          ))}
        </ul>
      </Section>

      <div className="start-foot">
        <p className="start-note">Nothing launches until you start it.</p>
        {result !== null ? (
          <p
            className="start-outcome"
            role="status"
            data-testid="start-outcome"
            {...(result.result === "armed" ? { "data-run": result.run ?? "", "data-device": result.device ?? "" } : {})}
          >
            {result.result === "armed" ? (
              <>
                <strong>{`Started · ${device === "" ? "unknown device" : device} · ${at}`}</strong>
                <span>{`Run ${result.run ?? "unknown"} is running`}</span>
              </>
            ) : (
              <strong>
                {result.result === "withdrawn"
                  ? `Withdrawn: ${result.reason ?? "the proposal was withdrawn"}`
                  : `Failed: ${result.reason ?? "the run could not start"}`}
              </strong>
            )}
          </p>
        ) : sent ? (
          <p className="start-outcome" role="status" data-testid="start-outcome">
            <strong>Arming…</strong>
          </p>
        ) : null}
        <div className="start-actions">
          <NotBuilt as="button" label="CLI" />
          <NotBuilt as="button" label="Change something" />
          {result !== null ? (
            result.result === "armed" ? <NotBuilt as="button" label="Open run view" /> : null
          ) : readOnly ? null : (
            <Button variant="primary" size="lg" icon="play" data-testid="start-workflow" disabled={busy || sent} onClick={() => void start()}>
              Start workflow
            </Button>
          )}
        </div>
        {problem ? (
          <p className="problem-title" role="alert">
            {problem}
          </p>
        ) : null}
      </div>
    </article>
  );
}
