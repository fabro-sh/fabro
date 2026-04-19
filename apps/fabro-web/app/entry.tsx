import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { createBrowserRouter, RouterProvider } from "react-router";
import { installRoutes } from "./install-router";
import { resolveFabroMode } from "./mode";
import { routes } from "./router";

declare global {
  interface Window {
    __FABRO_MODE__?: string;
  }
}

const router = createBrowserRouter(
  resolveFabroMode(window.__FABRO_MODE__) === "install" ? installRoutes : routes,
);
const rootElement = document.getElementById("root");

if (!rootElement) {
  throw new Error("Missing #root element");
}

createRoot(rootElement).render(
  <StrictMode>
    <RouterProvider router={router} />
  </StrictMode>,
);
