// The Markdown of a reply (11a): paragraphs, **bold**, *italic*, `inline code`, fenced code blocks, `-` and
// `1.` lists, and headings as bold lines. It builds React elements from text nodes only (ADR-0013,
// `r1.s5.w3`): it sets no markup string, it passes no HTML through and it draws no live link. HTML in a reply shows
// as literal text. A link shows as its text, with the URL in `title`: the app opens nothing by itself.
import type { ReactNode } from "react";

/** One inline mark: code, bold, italic or a link. The first alternative that matches at a place wins. */
const INLINE = /(`[^`\n]+`)|(\*\*[^*\n]+?\*\*)|(\*[^*\s][^*\n]*?\*)|(\[[^\]\n]+\]\((?:[^()\s]|\([^()\s]*\))+\))/g;

function inline(text: string, keyBase: string): ReactNode[] {
  const out: ReactNode[] = [];
  let last = 0;
  let n = 0;
  for (const match of text.matchAll(INLINE)) {
    const at = match.index ?? 0;
    if (at > last) out.push(text.slice(last, at));
    const key = `${keyBase}-${n++}`;
    const [whole, code, bold, italic, link] = match;
    if (code !== undefined) {
      out.push(
        <code key={key} className="md-code">
          {code.slice(1, -1)}
        </code>,
      );
    } else if (bold !== undefined) {
      out.push(<strong key={key}>{inline(bold.slice(2, -2), key)}</strong>);
    } else if (italic !== undefined) {
      out.push(<em key={key}>{inline(italic.slice(1, -1), key)}</em>);
    } else if (link !== undefined) {
      const close = link.indexOf("](");
      out.push(
        <span key={key} className="md-link" title={link.slice(close + 2, -1)}>
          {inline(link.slice(1, close), key)}
        </span>,
      );
    } else {
      out.push(whole);
    }
    last = at + whole.length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

const FENCE = /^\s*```/;
const HEADING = /^#{1,6}\s+(.*)$/;
const BULLET = /^\s*-\s+(.*)$/;
const NUMBERED = /^\s*\d+\.\s+(.*)$/;

/** Splits the text into blocks and renders each. */
export function Markdown(props: { text: string }) {
  const lines = props.text.split("\n");
  const blocks: ReactNode[] = [];
  let i = 0;
  const key = () => `b${blocks.length}`;

  while (i < lines.length) {
    const line = lines[i] ?? "";
    if (line.trim() === "") {
      i++;
    } else if (FENCE.test(line)) {
      // A fence that never closes runs to the end of the text.
      const body: string[] = [];
      i++;
      while (i < lines.length && !FENCE.test(lines[i] ?? "")) body.push(lines[i++] ?? "");
      i++;
      blocks.push(
        <pre key={key()} className="md-pre">
          <code>{body.join("\n")}</code>
        </pre>,
      );
    } else if (HEADING.test(line)) {
      const k = key();
      blocks.push(
        <p key={k} className="md-heading">
          <strong>{HEADING.exec(line)?.[1]}</strong>
        </p>,
      );
      i++;
    } else if (BULLET.test(line) || NUMBERED.test(line)) {
      const ordered = NUMBERED.test(line);
      const pattern = ordered ? NUMBERED : BULLET;
      const k = key();
      const items: ReactNode[] = [];
      while (i < lines.length && pattern.test(lines[i] ?? "")) {
        const text = pattern.exec(lines[i] ?? "")?.[1] ?? "";
        items.push(<li key={`${k}-${items.length}`}>{inline(text, `${k}-${items.length}`)}</li>);
        i++;
      }
      blocks.push(ordered ? <ol key={k}>{items}</ol> : <ul key={k}>{items}</ul>);
    } else {
      const k = key();
      const body: string[] = [];
      while (
        i < lines.length &&
        (lines[i] ?? "").trim() !== "" &&
        !FENCE.test(lines[i] ?? "") &&
        !HEADING.test(lines[i] ?? "") &&
        !BULLET.test(lines[i] ?? "") &&
        !NUMBERED.test(lines[i] ?? "")
      ) {
        body.push(lines[i++] ?? "");
      }
      blocks.push(<p key={k}>{inline(body.join("\n"), k)}</p>);
    }
  }
  return <div className="md">{blocks}</div>;
}
