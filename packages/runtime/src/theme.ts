/** Applies theme CSS and the colour scheme to the Spotify document. */

export type Scheme = "dark" | "light";

export const STYLE_ELEMENT_ID = "ghostify-theme";
export const SCHEME_ATTRIBUTE = "data-ghostify-scheme";

export class ThemeApplier {
  constructor(private readonly doc: Document) {}

  /** Replaces the current theme CSS. An empty string removes the theme. */
  applyCss(css: string): void {
    let element = this.doc.getElementById(STYLE_ELEMENT_ID);
    if (css === "") {
      element?.remove();
      return;
    }
    if (!element) {
      element = this.doc.createElement("style");
      element.id = STYLE_ELEMENT_ID;
      this.doc.head.appendChild(element);
    }
    if (element.textContent !== css) element.textContent = css;
  }

  applyScheme(scheme: Scheme): void {
    this.doc.documentElement.setAttribute(SCHEME_ATTRIBUTE, scheme);
  }

  currentCss(): string {
    return this.doc.getElementById(STYLE_ELEMENT_ID)?.textContent ?? "";
  }
}

export function isScheme(value: unknown): value is Scheme {
  return value === "dark" || value === "light";
}
