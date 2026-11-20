import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";
import { setTransport } from "./api/backend";

afterEach(() => {
  cleanup();
  setTransport(null);
});
