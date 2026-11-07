import "@fontsource-variable/geist";
import "@fontsource-variable/geist-mono";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./styles/tokens.css";
import "./styles/app.css";
import { createBridgeTransport } from "./transport/bridge";

// No StrictMode: it would run each page's first fetch twice in development, and the UI makes one
// call when a page opens.
const root = document.getElementById("root");
if (!root) throw new Error("index.html has no #root");
createRoot(root).render(<App transport={createBridgeTransport()} />);
