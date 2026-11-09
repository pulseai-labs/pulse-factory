// What the open stream says about itself, as one line: "Reconnecting…" with its reason while
// `pf-client` waits to try again, and, after a refusal or a server error, the text and Reconnect.
// A link failure shows the 3c banner instead (`state/chat.tsx`).
import { useChat } from "../state/chat";

export function StreamLine() {
  const { status, reconnect } = useChat();
  if (status.state === "reconnecting") {
    return (
      <p className="stream-line" role="status">
        {`Reconnecting… ${status.reason}`}
      </p>
    );
  }
  if (status.state === "error") {
    return (
      <p className="stream-line stream-error" role="alert">
        <span>{status.message}</span>
        <button className="btn" type="button" onClick={reconnect}>
          Reconnect
        </button>
      </p>
    );
  }
  return null;
}
