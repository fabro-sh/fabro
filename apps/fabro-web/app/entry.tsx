import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { createBrowserRouter, RouterProvider } from "react-router";
import { SWRConfig } from "swr";
import { AppearanceProvider } from "./components/appearance-provider";
import { FabroToaster } from "./components/toast";
import { useBuildVersionGuard } from "./hooks/use-build-version-guard";
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

function AppRuntime() {
  useBuildVersionGuard();

  return (
    <>
      <RouterProvider router={router} />
      <FabroToaster />
    </>
  );
}

createRoot(rootElement).render(
  <StrictMode>
    <AppearanceProvider>
      <SWRConfig
        value={{
          revalidateOnFocus: false,
          dedupingInterval: 2000,
          shouldRetryOnError: false,
        }}
      >
        <AppRuntime />
      </SWRConfig>
    </AppearanceProvider>
  </StrictMode>,
);
