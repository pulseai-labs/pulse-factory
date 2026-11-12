// The pair screen (3a). One field for the code, a device-name field and the command to run on the server.
import { useState, type FormEvent } from "react";
import { addressFromCode, hasCodePrefix, normalizeCode } from "../pair-code";
import {
  KEY_CHANGED_MESSAGE,
  TransportError,
  type PairedServer,
  type Transport,
} from "../transport/types";
import { Button, Eyebrow, Input } from "../ui";

export interface Notice {
  address: string;
  deviceId: string;
}

interface Problem {
  title: string;
  hint?: string;
}

const NEW_CODE_HINT = "Run pulse-factory device pair on the server for a new code.";

function nameProblem(name: string): Problem | null {
  const trimmed = name.trim();
  const length = [...trimmed].length;
  // eslint-disable-next-line no-control-regex
  const control = /[\u0000-\u001f\u007f-\u009f]/.test(name);
  if (length < 1 || length > 64 || control) {
    return { title: "A device name is 1 to 64 characters, with no control character." };
  }
  return null;
}

function problemFromError(err: TransportError, code: string): Problem {
  switch (err.kind) {
    case "code_refused":
      return { title: "The code is not accepted.", hint: NEW_CODE_HINT };
    case "unreachable": {
      const address = addressFromCode(code);
      return {
        title: address ? `Cannot reach ${address}.` : "Cannot reach the server.",
        hint: "Check that the server is running and that this Mac can reach it.",
      };
    }
    case "key_changed":
      return { title: KEY_CHANGED_MESSAGE, hint: NEW_CODE_HINT };
    default:
      // rate_limited and the rest: the server's own text, and the form stays.
      return { title: err.message };
  }
}

export function Pair(props: {
  transport: Transport;
  notice?: Notice;
  onPaired: (server: PairedServer) => void;
}) {
  const { transport, notice, onPaired } = props;
  const [code, setCode] = useState("");
  const [name, setName] = useState("MacBook");
  const [problem, setProblem] = useState<Problem | null>(null);
  const [busy, setBusy] = useState(false);
  const [paired, setPaired] = useState<PairedServer | null>(null);

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (busy) return;
    if (!hasCodePrefix(code)) {
      setProblem({ title: "That is not a pairing code.", hint: "A code starts with pf1-. " + NEW_CODE_HINT });
      return;
    }
    const nameIssue = nameProblem(name);
    if (nameIssue) {
      setProblem(nameIssue);
      return;
    }
    setProblem(null);
    setBusy(true);
    try {
      const server = await transport.pair(normalizeCode(code), name.trim());
      setPaired(server);
      setCode("");
    } catch (err) {
      setProblem(
        problemFromError(err instanceof TransportError ? err : new TransportError("server", "pairing failed"), code),
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="app app-pair">
      <header className="titlebar" role="banner">
        <span className="titlebar-server">
          <span className="dot dot-off" aria-hidden="true" />
          No server
        </span>
      </header>
      <main className="pair-main">
        {paired ? (
          <section className="pair-card" aria-labelledby="pair-done">
            <Eyebrow>Paired</Eyebrow>
            <h1 id="pair-done">Connected to {paired.address}</h1>
            <p className="muted">This Mac is paired. You only do this once.</p>
            <dl className="facts">
              <dt>Address</dt>
              <dd className="mono">{paired.address}</dd>
              <dt>This device</dt>
              <dd>{paired.device.name}</dd>
            </dl>
            <Button onClick={() => onPaired(paired)}>
              Open Pulse Factory
            </Button>
          </section>
        ) : (
          <section className="pair-card" aria-labelledby="pair-title">
            <h1 id="pair-title">Connect to your server</h1>
            <p className="muted">Pulse Factory runs on your Linux machine. Pair this Mac with it once.</p>
            {notice ? (
              <div className="notice" role="status">
                <p>This Mac is disconnected from {notice.address}.</p>
                <p>
                  The server still lists it. To cut it off there, run{" "}
                  <code>{`pulse-factory device revoke ${notice.deviceId}`}</code> on the server.
                </p>
              </div>
            ) : null}
            <form onSubmit={submit} noValidate>
              <div className="step-label">
                <span className="step-n">1 ·</span> On the server, run
              </div>
              <div className="command">
                <span className="prompt" aria-hidden="true">
                  $
                </span>
                <code>pulse-factory device pair</code>
              </div>
              <p className="hint">It prints one pairing code. The code works once, for 10 minutes.</p>

              <div className="step-label">
                <span className="step-n">2 ·</span> <label htmlFor="pf-code">Pairing code</label>
              </div>
              <Input
                mono
                id="pf-code"
                type="text"
                autoComplete="off"
                autoCapitalize="off"
                spellCheck={false}
                placeholder="pf1-…"
                value={code}
                disabled={busy}
                onChange={(e) => setCode(e.target.value)}
              />

              <div className="step-label">
                <span className="step-n">3 ·</span> <label htmlFor="pf-name">Device name</label>
              </div>
              <Input
                id="pf-name"
                type="text"
                autoComplete="off"
                value={name}
                disabled={busy}
                onChange={(e) => setName(e.target.value)}
              />

              {problem ? (
                <div className="problem" role="alert">
                  <p className="problem-title">{problem.title}</p>
                  {problem.hint ? <p className="problem-hint">{problem.hint}</p> : null}
                </div>
              ) : null}

              <Button variant="primary" size="xl" type="submit" disabled={busy}>
                {busy ? "Pairing…" : "Pair"}
              </Button>
            </form>
          </section>
        )}
      </main>
    </div>
  );
}
