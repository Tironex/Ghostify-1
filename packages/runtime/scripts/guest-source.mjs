import { buildSync } from "esbuild";

/** Compiles the code that runs inside every plugin sandbox into one string. */
export function guestSource() {
  const result = buildSync({
    entryPoints: ["src/plugins/guest.ts"],
    bundle: true,
    minify: true,
    format: "iife",
    target: "chrome100",
    write: false,
    legalComments: "none",
  });
  const output = result.outputFiles[0];
  if (!output) throw new Error("guest bundle is empty");
  return output.text;
}
