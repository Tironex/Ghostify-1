import { defineConfig } from "vitest/config";
import { guestSource } from "./scripts/guest-source.mjs";

export default defineConfig({
  define: { __GUEST_SOURCE__: JSON.stringify(guestSource()) },
  test: {
    environment: "jsdom",
    // The sandbox tests run the real guest script, which needs script execution.
    environmentOptions: { jsdom: { runScripts: "dangerously" } },
    include: ["test/**/*.test.ts"],
  },
});
