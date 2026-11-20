import type { SVGProps } from "react";

export function Crown(props: SVGProps<SVGSVGElement>) {
  return (
    <svg viewBox="0 0 48 32" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinejoin="round" strokeLinecap="round" aria-hidden="true" {...props}>
      <path d="M5 27 L3 9 L15 18 L24 4 L33 18 L45 9 L43 27 Z" />
      <path d="M6 27 H42" />
      <circle cx="3" cy="8" r="1.6" fill="currentColor" />
      <circle cx="24" cy="3.2" r="1.6" fill="currentColor" />
      <circle cx="45" cy="8" r="1.6" fill="currentColor" />
    </svg>
  );
}

/** The Ghostify wordmark with its crown. */
export function Logo({ className = "" }: { className?: string }) {
  return (
    <div className={`flex flex-col items-start ${className}`}>
      <Crown className="ml-5 h-4 w-6 text-zinc-200" />
      <span className="ghost-wordmark -mt-0.5 text-2xl leading-none text-white" aria-label="Ghostify">
        GHOSTIFY
      </span>
    </div>
  );
}

/** A simple torii gate silhouette, used as banner decoration. */
export function Torii(props: SVGProps<SVGSVGElement>) {
  return (
    <svg viewBox="0 0 120 100" fill="currentColor" aria-hidden="true" {...props}>
      <path d="M2 20 Q60 6 118 20 L113 29 Q60 17 7 29 Z" />
      <rect x="16" y="36" width="88" height="7" rx="1" />
      <rect x="55" y="26" width="10" height="11" />
      <path d="M24 26 H36 L38 100 H22 Z" />
      <path d="M84 26 H96 L98 100 H82 Z" />
    </svg>
  );
}

type IconName = "home" | "palette" | "plug" | "store" | "spark" | "gear" | "prev" | "next" | "play" | "pause" | "mini";

const paths: Record<IconName, string> = {
  home: "M3 11 12 3l9 8v10h-6v-6H9v6H3z",
  palette: "M12 3a9 9 0 1 0 0 18c1.5 0 2-1 1.4-2-.6-1 0-2.2 1.3-2.2H17a4 4 0 0 0 4-4C21 6.6 17 3 12 3zM7.5 12a1.3 1.3 0 1 1 0-2.6 1.3 1.3 0 0 1 0 2.6zm3-4a1.3 1.3 0 1 1 0-2.6 1.3 1.3 0 0 1 0 2.6zm4.5 0a1.3 1.3 0 1 1 0-2.6 1.3 1.3 0 0 1 0 2.6z",
  plug: "M9 2v6M15 2v6M6 8h12v4a6 6 0 0 1-12 0zM12 18v4",
  store: "M4 9l1.5-5h13L20 9M4 9h16v11H4zM4 9a2.7 2.7 0 0 0 5.3 0 2.7 2.7 0 0 0 5.4 0A2.7 2.7 0 0 0 20 9M10 20v-6h4v6",
  spark: "M12 2l2.2 6.3L21 10l-6.8 1.7L12 18l-2.2-6.3L3 10l6.8-1.7zM19 16l.9 2.1L22 19l-2.1.9L19 22l-.9-2.1L16 19l2.1-.9z",
  gear: "M12 8.5a3.5 3.5 0 1 0 0 7 3.5 3.5 0 0 0 0-7zM19.4 13a7.5 7.5 0 0 0 0-2l2-1.6-2-3.4-2.4.9a7.5 7.5 0 0 0-1.7-1L15 3.3h-4l-.3 2.6a7.5 7.5 0 0 0-1.7 1l-2.4-.9-2 3.4 2 1.6a7.5 7.5 0 0 0 0 2l-2 1.6 2 3.4 2.4-.9c.5.4 1.1.7 1.7 1l.3 2.6h4l.3-2.6c.6-.3 1.2-.6 1.7-1l2.4.9 2-3.4z",
  prev: "M6 5h2v14H6zM20 5v14L9 12z",
  next: "M16 5h2v14h-2zM4 5l11 7L4 19z",
  play: "M7 4v16l13-8z",
  pause: "M6 4h4v16H6zM14 4h4v16h-4z",
  mini: "M3 5h18v14H3zM12 12h7v5h-7z",
};

const stroked = new Set<IconName>(["plug", "store"]);

export function Icon({ name, className = "h-5 w-5" }: { name: IconName; className?: string }) {
  const outline = stroked.has(name);
  return (
    <svg
      viewBox="0 0 24 24"
      className={className}
      aria-hidden="true"
      fill={outline ? "none" : "currentColor"}
      stroke={outline ? "currentColor" : "none"}
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d={paths[name]} />
    </svg>
  );
}
