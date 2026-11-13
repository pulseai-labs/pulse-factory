// The pair screen (11j, over 3a). The title bar holds only the server chip ("No server", no menu) and New tab
// as a function not built yet. A centred card: the command to run on the server with its Copy button, one
// field for the code and the device name (O3: no address field, the code carries it). After a pairing the
// chip names the server with the green dot, and the card says "Connected to <name>".
import { useState, type FormEvent } from "react";
import { addressFromCode, hasCodePrefix, normalizeCode } from "../pair-code";
import {
  KEY_CHANGED_MESSAGE,
  TransportError,
  type PairedServer,
  type Transport,
} from "../transport/types";
import { Button, Icon, Input, NotBuilt } from "../ui";

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

  const [copied, setCopied] = useState(false);
  const serverName = paired ? (paired.server?.name ?? paired.address) : null;

  async function copy() {
    try {
      await navigator.clipboard.writeText("pulse-factory device pair");
      setCopied(true);
    } catch {
      // No clipboard: the command is on screen to copy by hand.
    }
  }

  return (
    <div className="app app-pair">
      <header className="titlebar" role="banner" data-tauri-drag-region>
        <span className="tb-chip tb-chip-static" title={paired ? undefined : "Pair a server"}>
          <span className={`tb-dot ${paired ? "tb-dot-up" : "tb-dot-off"}`} aria-hidden="true" />
          {serverName ?? "No server"}
          <span className="tb-chip-caret" aria-hidden="true">
            <Icon name="chevrons-up-down" size={12} />
          </span>
        </span>
        <span className="tb-divider" aria-hidden="true" />
        <div className="tb-strip">
          <NotBuilt as="button" compact icon="plus" label="New tab" />
        </div>
        <div className="tb-spacer" data-tauri-drag-region />
      </header>
      <main className="pair-main">
        {paired && serverName ? (
          <section className="pair-card" aria-labelledby="pair-done">
            <div className="pair-head">
              <span className="pair-tile" aria-hidden="true">
                <Icon name="check" size={18} />
              </span>
              {/* The heading names the address, not the server's name. The journey lines d19 and d23 (`e2e/chat.spec.ts`,
                  `e2e/start-card.spec.ts`, `e2e/needs-you.spec.ts`; round-3 owners, AC-13 forbids this item to edit them)
                  find it as `Connected to 127.0.0.1:<port>`. The canvas (11j) says "Connected to <name>": once those specs
                  read the name, this line takes `serverName`. The name shows in the lede, the Server fact and the chip. */}
              <h1 id="pair-done">Connected to {paired.address}</h1>
              <p className="pair-lede">{`This Mac is paired and trusts ${serverName}’s key. You only do this once.`}</p>
            </div>
            <dl className="pair-facts">
              <div className="pair-fact">
                <dt>Server</dt>
                <dd>
                  <span className="tb-dot tb-dot-up" aria-hidden="true" />
                  {serverName}
                </dd>
              </div>
              <div className="pair-fact">
                <dt>Address</dt>
                <dd className="mono">{paired.address}</dd>
              </div>
              <div className="pair-fact">
                <dt>This device</dt>
                <dd>{paired.device.name}</dd>
              </div>
            </dl>
            <div className="pair-actions">
              <NotBuilt as="button" label="Pair another code" />
              <span className="pair-actions-gap" />
              <Button variant="primary" size="lg" onClick={() => onPaired(paired)}>
                Open Pulse Factory
              </Button>
            </div>
          </section>
        ) : (
          <section className="pair-card" aria-labelledby="pair-title">
            <div className="pair-head">
              <span className="pair-tile" aria-hidden="true">
                <Icon name="activity" size={18} />
              </span>
              <h1 id="pair-title">Connect to your server</h1>
              <p className="pair-lede">Pulse Factory runs on your Linux machine. Pair this Mac with it once.</p>
            </div>
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
              <div className="pair-field">
                <span className="step-label">
                  <span className="step-n">1 ·</span> On the server, run
                </span>
                <div className="command">
                  <span className="prompt" aria-hidden="true">
                    $
                  </span>
                  <code>pulse-factory device pair</code>
                  <button className="command-copy" type="button" title="Copy command" aria-label="Copy command" onClick={() => void copy()}>
                    <Icon name={copied ? "check" : "copy"} size={13} />
                    {copied ? "Copied" : "Copy"}
                  </button>
                </div>
                <p className="hint">
                  It prints one code that starts with <code>pf1-</code>. The code carries the server’s address and the key this Mac will trust.
                </p>
              </div>

              <div className="pair-field">
                <label className="step-label" htmlFor="pf-code">
                  <span className="step-n">2 ·</span> Paste the code
                </label>
                <Input
                  mono
                  id="pf-code"
                  aria-label="Pairing code"
                  type="text"
                  autoComplete="off"
                  autoCapitalize="off"
                  spellCheck={false}
                  placeholder="pf1-…"
                  value={code}
                  disabled={busy}
                  onChange={(e) => setCode(e.target.value)}
                />
              </div>

              <div className="pair-field">
                <label className="step-label" htmlFor="pf-name">
                  <span className="step-n">3 ·</span> Name this Mac
                </label>
                <Input
                  id="pf-name"
                  aria-label="Device name"
                  type="text"
                  autoComplete="off"
                  placeholder="MacBook"
                  value={name}
                  disabled={busy}
                  onChange={(e) => setName(e.target.value)}
                />
                <p className="hint">Shown on answers you give from this Mac, like “MacBook · 14:08”.</p>
              </div>

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
