// What the open stream says about itself, as one line: after a refusal or a server error, the text and
// Reconnect. A stream that `pf-client` is retrying by itself, and a link failure, show the not-connected
// banner instead (`components/Banner.tsx`).
import { useChat } from "../state/chat";
import { Button } from "../ui";

export function StreamLine() {
  const { status, reconnect } = useChat();
  if (status.state === "error") {
    return (
      <p className="stream-line stream-error" role="alert">
        <span>{status.message}</span>
        <Button onClick={reconnect}>
          Reconnect
        </Button>
      </p>
    );
  }
  return null;
}
