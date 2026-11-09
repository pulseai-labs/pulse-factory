// A tool call's input or output as text, with the mark that the adapter cut it.
import type { ToolTextRecord } from "@pulse-factory/api-client";

export function ToolText(props: { label: string; value: ToolTextRecord | null }) {
  const { label, value } = props;
  if (value === null) return null;
  return (
    <div className="tool-text">
      <span className="tool-label">{label}</span>
      <pre aria-label={label}>{value.text}</pre>
      {value.truncated ? <span className="muted">{`truncated, ${value.bytes} bytes`}</span> : null}
    </div>
  );
}
