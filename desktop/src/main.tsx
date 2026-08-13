import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";

import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/shell.css";
import "./styles/controls.css";
import "./styles/views.css";

// The browser context menu is a web tell; a desktop app supplies its own.
window.addEventListener("contextmenu", (e) => e.preventDefault());

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
