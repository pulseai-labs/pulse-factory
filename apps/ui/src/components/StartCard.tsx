// The start card (11c): a proposal that the Orchestrator chat posted, with the facts of the plan, and one
// button. **Start workflow** is one press (G1): it asks the server for a one-use code for this proposal and
// this plan, and at once arms the run with it. The code lives in this component's state only: it is never
// shown, stored, logged or put in a URL. The card learns the outcome from the `start_result` item on the
// chat stream the shell already holds, so it has no timer and no stream of its own.
//
// r1.s6.w2: the head has 11c's chip row (the spine ID, the spine branch, an arrow, the base: KL-4) and the
// merge sentence as 11c writes it (P5); the limits are three icon tiles, and the seeds, relaunch and loops are
// the group's title; the item count is at the right of the Work items label row; after the start the card
// ends in the Started block, with the quiet "Open run view" link, and the note and the two stubs leave.
import type { ArmMerge, ChatItemBody } from "@pulse-factory/api-client";
import { Fragment, useState } from "react";
import { asTransportError, useConnection } from "../state/connection";
import { Button, Chip, Eyebrow, Icon, NotBuilt, modelName, type IconName } from "../ui";
import { timeText } from "./chat-format";
import { useCommandId } from "./command-id";

type Card = Extract<ChatItemBody, { kind: "start_card" }>;
type Result = Extract<ChatItemBody, { kind: "start_result" }>;

/** One tile of the Limits group: 11c's icon tile, the value, and the label. */
function Tile(props: { value: string | number; label: string; icon: IconName; tone: "blue" | "violet" | "neutral"; name?: string }) {
  return (
    <div className="limit-tile" {...(props.name ? { role: "group", "aria-label": props.name } : {})}>
      <span className={`limit-icon limit-icon-${props.tone}`}>
        <Icon name={props.icon} size={14} />
      </span>
      <span className="limit-text">
        <span className="limit-value">{props.value}</span>
        <span className="limit-label">{props.label}</span>
      </span>
    </div>
  );
}

/**
 * One merge as 11c writes it (ruling P5), with the base in a code chip. The merge rule is not a field of
 * `ArmMerge`: the server writes it into `sentence`, in one of two forms only (`arm.rs:315-322`):
 * "item PRs merge into <base> on green checks with no ack" reads "Item PRs merge into `<base>` without
 * asking.", and "the <track> PR merges into <base> only on the claim's ack" reads "The <track> PR into
 * `<base>` waits for you." Any other sentence shows unchanged, as the server wrote it. The base and the track
 * are the merge's own fields.
 */
function MergeSentence(props: { merge: ArmMerge }) {
  const { sentence, base, track } = props.merge;
  if (sentence.startsWith("item PRs merge into ")) {
    return (
      <span>
        Item PRs merge into <code className="merge-code">{base}</code> without asking.
      </span>
    );
  }
  if (sentence.endsWith(" only on the claim's ack")) {
    return (
      <span>
        {`The ${track} PR into `}
        <code className="merge-code">{base}</code> waits for you.
      </span>
    );
  }
  return <span>{sentence}</span>;
}

function Section(props: { name: string; aside?: string; children?: React.ReactNode }) {
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

  // The spine's branch (KL-4): the base of the first merge whose base is not the default branch. With no such
  // merge the row shows the spine ID only. The base the row ends in is the plan's default branch.
  const spineBranch = s.merges.find((m) => m.base !== s.default_branch)?.base ?? null;

  // The seeds, the relaunch limit and the loops: the Limits group's title (d24-1, d24-4).
  const others = [
    ...s.caps.seeds.map((x) => `seed ${x.id}: at most ${x.max}`),
    `relaunch: ${s.caps.relaunch}`,
    ...s.caps.loops.map((x) => `loop ${x.track} ${x.from} to ${x.to}: at most ${x.max}`),
  ].join(" · ");

  // The run is armed: the card ends in the Started block, and the note and the two stubs leave (W1-31).
  const armed = result !== null && result.result === "armed" ? result : null;
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
          <span className="start-project">{card.project_name}</span>
          {s.paper ? (
            <Chip tone="mint" role="group" aria-label="Paper env" title="No model, no spend">
              Paper
            </Chip>
          ) : null}
        </div>

        <div className="start-branches" role="group" aria-label="Spine and branches">
          <Chip mono>{s.spine}</Chip>
          {spineBranch === null ? null : (
            <>
              <Chip mono tone="violet">
                {spineBranch}
              </Chip>
              <span className="branch-arrow">
                <Icon name="chevron-right" size={14} />
              </span>
              <Chip mono tone="mint">
                {s.default_branch}
              </Chip>
            </>
          )}
        </div>

        <div className="limits" role="group" aria-label="Limits" title={others}>
          <div className="limit-tiles">
            <Tile value={s.caps.model_sessions} label="Model sessions" icon="sparkles" tone="violet" />
            <Tile value={s.caps.cold_builds} label="Cold builds" icon="zap" tone="blue" />
            {s.most_launches === null ? null : (
              <Tile value={s.most_launches} label="Most sessions" icon="layers" tone="neutral" name="Most sessions" />
            )}
          </div>
          <p className="limit-note">The run stops and asks you when it reaches a limit.</p>
        </div>
        {s.merges.length === 0 ? null : (
          <div className="merge-line" role="group" aria-label="Merge line">
            <Icon name="git-branch" size={14} />
            <span>
              {s.merges.map((m, i) => (
                <Fragment key={i}>
                  {i === 0 ? null : " "}
                  <MergeSentence merge={m} />
                </Fragment>
              ))}
            </span>
          </div>
        )}
      </div>

      {s.items === null ? null : <Section name="Work items" aside={`${s.items} work ${s.items === 1 ? "item" : "items"}`} />}

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

      {armed ? (
        <div
          className="start-started"
          role="status"
          data-testid="start-outcome"
          data-run={armed.run ?? ""}
          data-device={armed.device ?? ""}
        >
          <span className="started-icon">
            <Icon name="check" size={15} />
          </span>
          <span className="started-title">Started</span>
          <span className="started-meta">{`${device === "" ? "unknown device" : device} · ${at}`}</span>
          <span className="head-spacer" />
          <span className="started-run">
            Run <b>{armed.run ?? "unknown"}</b> is running
          </span>
          <NotBuilt as="button" label="Open run view" />
        </div>
      ) : (
        <div className="start-foot">
          <p className="start-note">Nothing launches until you start it.</p>
          {result !== null ? (
            <p className="start-outcome" role="status" data-testid="start-outcome">
              <strong>
                {result.result === "withdrawn"
                  ? `Withdrawn: ${result.reason ?? "the proposal was withdrawn"}`
                  : `Failed: ${result.reason ?? "the run could not start"}`}
              </strong>
            </p>
          ) : sent ? (
            <p className="start-outcome" role="status" data-testid="start-outcome">
              <strong>Arming…</strong>
            </p>
          ) : null}
          <div className="start-actions">
            <NotBuilt as="button" label="CLI" />
            <NotBuilt as="button" label="Change something" />
            {result !== null || readOnly ? null : (
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
      )}
    </article>
  );
}
