// One row of "Answered today" (11d, r1.s5.w4): "<spine> · <run>" with "asked hh:mm", the question, the answer,
// and who answered with the time. The claim line of the old page is gone (G1): the claim shows here only, as
// the holder of the generation that answered.
import type { ClaimRecord, RequestDetail, RequestSummary } from "@pulse-factory/api-client";
import { clock } from "../components/format";
import { parseTime } from "../components/chat-format";
import { Icon } from "../ui";
import type { Done } from "./useConfirmedAct";
import { firstLine, fieldOf, questionOf, shortSha } from "./format";

/** The device this app is paired as: its ID and its name. */
export interface DeviceName {
  id: string;
  name: string;
}

/** The answer as the row says it: the text, "Merge approved", "Resumed at <stage>" or "Withdrawn". */
export function answerWords(request: RequestSummary, detail: RequestDetail | undefined, done: Done | undefined): { what: string; detail: string | null } | null {
  if (done) return { what: done.what, detail: done.detail };
  if (request.status === "withdrawn") return { what: "Withdrawn", detail: null };
  if (request.kind === "merge_ack") {
    const sha = fieldOf("head_sha", request, detail);
    return { what: "Merge approved", detail: sha === null ? null : `Merged at ${shortSha(sha)}` };
  }
  const answer = detail?.answer;
  if (typeof answer === "object" && answer !== null) {
    const a = answer as Record<string, unknown>;
    if (a.verb === "clear") return { what: `Resumed at ${request.stage}`, detail: null };
    if (typeof a.text === "string") return { what: a.text, detail: null };
  }
  return null;
}

/** Who answered: this device's name, a CLI claim's holder, or the claim's generation. */
export function whoWords(
  detail: RequestDetail | undefined,
  done: Done | undefined,
  claim: ClaimRecord | null,
  device: DeviceName,
): string | null {
  if (done) return done.device;
  const generation = detail?.answer_generation ?? null;
  if (generation === null) return null;
  if (claim !== null && claim.generation === generation) {
    if (claim.device === null) return `CLI · claim ${claim.holder}`;
    return claim.device === device.id ? device.name : claim.holder;
  }
  return `claim ${generation}`;
}

/** The question of an answered request, from any body held for it. */
export function questionText(request: RequestSummary, body: RequestDetail | undefined): string {
  const { stop, gaps } = questionOf(body?.body);
  if (stop !== null) return stop;
  if (gaps.length > 0) return gaps.map((g) => `${g.section} ${g.question}`.trim()).join(" · ");
  if (request.kind === "merge_ack") {
    const branch = fieldOf("head_branch", request, body);
    const base = fieldOf("base", request, body);
    if (branch !== null && base !== null) return `Merge ${branch} into ${base}?`;
  }
  return firstLine(request.reason);
}

export function AnsweredRow(props: {
  request: RequestSummary;
  /** A body held for this request at any revision: the question. */
  body: RequestDetail | undefined;
  /** The detail at the revision it answered at: the answer and the generation. */
  detail: RequestDetail | undefined;
  done: Done | undefined;
  notice: string | undefined;
  spine: string | null;
  claim: ClaimRecord | null;
  device: DeviceName;
}) {
  const { request, body, detail, done, notice, spine, claim, device } = props;
  const asked = parseTime(request.opened_at);
  const answeredAt = parseTime(request.answered_at);
  const answer = answerWords(request, detail, done);
  const who = whoWords(detail, done, claim, device);
  const time = answeredAt === null ? null : clock(answeredAt);
  const tail = [who, time].filter((s): s is string => s !== null && s !== "").join(" · ");
  return (
    <li className="ny-ans" aria-label={`Answered ${request.id}`}>
      <div className="ny-head">
        <span className="ny-icon ny-icon-run">
          <Icon name={request.kind === "merge_ack" ? "git-branch" : "activity"} size={12} />
        </span>
        <span className="ny-name">{spine ?? request.run}</span>
        {spine !== null ? <span className="ny-where">{`· ${request.run}`}</span> : null}
        <span className="ny-spacer" />
        <span className="ny-age">{asked === null ? "" : `asked ${clock(asked)}`}</span>
      </div>
      <p className="ny-ans-question">{questionText(request, body ?? detail)}</p>
      <div className="ny-ans-line">
        <span className="ny-ans-icon">
          <Icon name="check" size={13} />
        </span>
        {answer !== null ? <span className="ny-ans-what">{answer.what}</span> : <span className="ny-ans-what">{notice ?? "Answered"}</span>}
        {answer?.detail ? <span className="ny-ans-detail">{answer.detail}</span> : null}
        <span className="ny-spacer" />
        <span className="ny-age">{tail}</span>
      </div>
    </li>
  );
}
