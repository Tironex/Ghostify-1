/** Makes plugin-supplied CSS harmless: no remote loads, no imports. */

export const MAX_PLUGIN_CSS = 200 * 1024;

export function sanitizeCss(css: string): string {
  if (css.length > MAX_PLUGIN_CSS) throw new Error(`css is larger than ${MAX_PLUGIN_CSS} characters`);
  return css
    .replace(/@import\b[^;]*;?/gi, "")
    .replace(/url\(\s*(?:"([^"]*)"|'([^']*)'|([^)"'\s]*))\s*\)/gi, (whole, a?: string, b?: string, c?: string) => {
      const target = (a ?? b ?? c ?? "").trim().toLowerCase();
      return target.startsWith("data:") || target.startsWith("#") ? whole : 'url("")';
    })
    .replace(/expression\s*\(|-moz-binding|behavior\s*:|javascript\s*:/gi, "");
}
