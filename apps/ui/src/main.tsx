import "@fontsource-variable/geist";
import "@fontsource-variable/geist-mono";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./styles/tokens.css";
import "./styles/app-tokens.css";
import "./ui/ui.css";
import "./styles/app.css";
import "./shell/shell.css";
import { isTauri } from "@tauri-apps/api/core";
import { createBrowserNotifier } from "./notify/notifier";
import { createTauriNotifier } from "./notify/tauri-notifier";
import { createBridgeTransport } from "./transport/bridge";
import { createTauriTransport } from "./transport/tauri";

// No StrictMode: it would run each page's first fetch twice in development, and the UI makes one
// call when a page opens.
const root = document.getElementById("root");
if (!root) throw new Error("index.html has no #root");
// Inside the Mac app the transport is the shell's Rust side. Elsewhere it is the test bridge, which
// serves this bundle and speaks the same protocol (docs/contracts/ui-transport.md).
const inTauri = isTauri();
const transport = inTauri ? createTauriTransport() : createBridgeTransport();
// The notifier is chosen as the transport is: the notification plugin inside Tauri, the page's own `Notification` elsewhere.
const notifier = inTauri ? createTauriNotifier() : createBrowserNotifier();
createRoot(root).render(<App transport={transport} notifier={notifier} />);
