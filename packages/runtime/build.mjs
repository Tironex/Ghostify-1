import { build } from "esbuild";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { guestSource } from "./scripts/guest-source.mjs";

await mkdir("dist", { recursive: true });
await build({
  entryPoints: ["src/main.ts"],
  bundle: true,
  minify: true,
  format: "iife",
  target: "chrome100",
  outfile: "dist/loader.js",
  legalComments: "none",
  define: { __GUEST_SOURCE__: JSON.stringify(guestSource()) },
});
await writeFile("dist/loader.css", await readFile("src/loader.css", "utf8"));
console.log("built dist/loader.js and dist/loader.css");
