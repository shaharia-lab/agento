import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";

import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/shell.css";
import "./styles/controls.css";
import "./styles/views.css";

// The browser context menu is a web tell — suppress it on chrome surfaces.
// But inside editable fields and over selected text the webview's own menu is
// the only Copy/Paste affordance the app has, so those keep it.
window.addEventListener("contextmenu", (e) => {
  const t = e.target as HTMLElement | null;
  const editable =
    !!t &&
    (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable);
  const hasSelection = !!window.getSelection()?.toString();
  if (editable || hasSelection) return;
  e.preventDefault();
});

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
