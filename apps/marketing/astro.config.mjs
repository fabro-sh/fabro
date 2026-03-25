import { defineConfig } from "astro/config";
import react from "@astrojs/react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  integrations: [react()],
  redirects: {
    "/discord": {
      status: 302,
      destination:
        "https://discord.gg/PxwJ8DQHNE",
    },
    "/docs": {
      status: 302,
      destination: "https://docs.fabro.sh",
    },
  },
  vite: {
    plugins: [tailwindcss()],
    server: {
      allowedHosts: [".ngrok-free.app"],
    },
  },
});
