// The start: `paired()` decides. No server shows the pair screen (3a). A server shows the shell on
// the Projects page.
import { useCallback, useEffect, useState } from "react";
import { Pair, type Notice } from "./screens/Pair";
import { Shell } from "./screens/Shell";
import { ConnectionProvider } from "./state/connection";
import type { PairedServer, Transport } from "./transport/types";

type View =
  | { name: "loading" }
  | { name: "start_error"; message: string }
  | { name: "pair"; notice?: Notice }
  | { name: "shell"; server: PairedServer };

export function App(props: { transport: Transport }) {
  const { transport } = props;
  const [view, setView] = useState<View>({ name: "loading" });
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    let live = true;
    transport.paired().then(
      (server) => {
        if (live) setView(server ? { name: "shell", server } : { name: "pair" });
      },
      (err: unknown) => {
        if (live) {
          setView({ name: "start_error", message: err instanceof Error ? err.message : "cannot start" });
        }
      },
    );
    return () => {
      live = false;
    };
  }, [transport, attempt]);

  const onForgotten = useCallback((server: PairedServer) => {
    setView({ name: "pair", notice: { address: server.address, deviceId: server.device.id } });
  }, []);
  const onPairAgain = useCallback(() => setView({ name: "pair" }), []);

  switch (view.name) {
    case "loading":
      return <div className="app app-loading" aria-busy="true" />;
    case "start_error":
      return (
        <div className="app app-pair">
          <main className="pair-main">
            <section className="pair-card">
              <h1>Pulse Factory cannot start</h1>
              <p className="problem-title" role="alert">
                {view.message}
              </p>
              <button
                className="btn"
                type="button"
                onClick={() => {
                  setView({ name: "loading" });
                  setAttempt((n) => n + 1);
                }}
              >
                Try again
              </button>
            </section>
          </main>
        </div>
      );
    case "pair":
      return (
        <Pair
          transport={transport}
          notice={view.notice}
          onPaired={(server) => setView({ name: "shell", server })}
        />
      );
    case "shell":
      return (
        <ConnectionProvider
          key={`${view.server.address}/${view.server.device.id}`}
          transport={transport}
          server={view.server}
          onForgotten={onForgotten}
          onPairAgain={onPairAgain}
        >
          <Shell />
        </ConnectionProvider>
      );
  }
}
