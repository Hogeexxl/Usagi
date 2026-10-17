import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import "../index.css";
import { McpDashboardApp } from "./McpDashboardApp";

const rootElement = document.getElementById("root");
if (!rootElement) throw new Error("Missing #root element");

createRoot(rootElement).render(
  <StrictMode>
    <McpDashboardApp />
  </StrictMode>,
);
