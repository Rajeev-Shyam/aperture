//! Overlay entry point (doc 11 §2). Mounts the transparent React tree into
//  #root. The window's transparency / always-on-top / taskbar-skip /
//  click-through flags are Win32 settings owned by the Rust shell (src-tauri),
//  NOT this document.

import React from "react";
import ReactDOM from "react-dom/client";

import App from "./App";

// Token + component styles. design-tokens.css must load first so the cascade
// has the CSS vars before bubble.css references them.
import "./styles/design-tokens.css";
import "./styles/bubble.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
