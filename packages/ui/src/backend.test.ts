import { BackendError, call, errorText, pickPath, setTransport } from "./api/backend";

describe("backend", () => {
  it("fails with a clear error outside the desktop app", async () => {
    await expect(call("app.info")).rejects.toMatchObject({ code: "unavailable" });
  });

  it("passes method and params to the transport", async () => {
    const seen: unknown[] = [];
    setTransport(async (method, params) => (seen.push([method, params]), 42));
    expect(await call("x.y", { a: 1 })).toBe(42);
    expect(await call("z")).toBe(42);
    expect(seen).toEqual([["x.y", { a: 1 }], ["z", null]]);
  });

  it("turns Tauri error objects, strings and exceptions into BackendError", async () => {
    setTransport(async () => { throw { code: "conflict", message: "already there" }; });
    await expect(call("a")).rejects.toMatchObject({ code: "conflict", message: "already there" });
    setTransport(async () => { throw "plain"; });
    await expect(call("a")).rejects.toMatchObject({ code: "internal", message: "plain" });
    setTransport(async () => { throw 5; });
    await expect(call("a")).rejects.toMatchObject({ code: "internal", message: "Unexpected error" });
    setTransport(async () => { throw new BackendError("not_found", "gone"); });
    await expect(call("a")).rejects.toBeInstanceOf(BackendError);
  });

  it("uses the Tauri invoke command when present", async () => {
    const invoke = vi.fn(async () => "ok");
    window.__TAURI__ = { core: { invoke } };
    try {
      expect(await call("app.info", { q: 1 })).toBe("ok");
      expect(invoke).toHaveBeenCalledWith("ghostify_call", { method: "app.info", params: { q: 1 } });
    } finally {
      delete window.__TAURI__;
    }
  });

  it("picks paths through the dialog plugin and returns null without it", async () => {
    expect(await pickPath()).toBeNull();
    const open = vi.fn<(options: Record<string, unknown>) => Promise<string | null>>(async () => "/home/me/theme");
    window.__TAURI__ = { dialog: { open } };
    try {
      expect(await pickPath({ directory: true })).toBe("/home/me/theme");
      expect(open).toHaveBeenCalledWith({ directory: true, multiple: false });
      open.mockResolvedValueOnce(null);
      expect(await pickPath()).toBeNull();
    } finally {
      delete window.__TAURI__;
    }
  });

  it("describes errors as text", () => {
    expect(errorText(new Error("boom"))).toBe("boom");
    expect(errorText("x")).toBe("x");
  });
});
