import { serve } from "bun";
import index from "./index.html";

const BACKEND_URL = process.env.BACKEND_URL ?? "http://localhost:3000";

function proxyToBackend(req: Request): Promise<Response> {
  const url = new URL(req.url);
  const backendPath = url.pathname.replace(/^\/api/, "");
  const target = `${BACKEND_URL}${backendPath}${url.search}`;

  return fetch(target, {
    method: req.method,
    headers: req.headers,
    body: req.body,
    // @ts-expect-error Bun supports duplex on fetch
    duplex: "half",
  });
}

const server = serve({
  port: 5173,

  routes: {
    "/api/*": proxyToBackend,
    "/*": index,
  },

  development: process.env.NODE_ENV !== "production" && {
    hmr: true,
    console: true,
  },
});

console.log(`Server running at ${server.url}`);
