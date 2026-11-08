import "@fontsource-variable/geist";
import "@fontsource-variable/geist-mono";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./styles/tokens.css";
import "./styles/app.css";
import { isTauri } from "@tauri-apps/api/core";
import { createBridgeTransport } from "./transport/bridge";
import { createTauriTransport } from "./transport/tauri";

// No StrictMode: it would run each page's first fetch twice in development, and the UI makes one
// call when a page opens.
const root = document.getElementById("root");
if (!root) throw new Error("index.html has no #root");
// Inside the Mac app the transport is the shell's Rust side. Elsewhere it is the test bridge, which
// serves this bundle and speaks the same protocol (docs/contracts/ui-transport.md).
const transport = isTauri() ? createTauriTransport() : createBridgeTransport();
createRoot(root).render(<App transport={transport} />);
