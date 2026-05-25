import { describe, expect, test } from "bun:test";

import {
  buildTerminalWebSocketUrl,
  parseTerminalServerMessage,
  sandboxStatusDetail,
  TERMINAL_DOCK_CLEARANCE_CLASS,
  terminalAccessCommandLabel,
} from "./terminal-view-helpers";

function locationLike(url: string): Location {
  return new URL(url) as unknown as Location;
}

describe("terminal view helpers", () => {
  test("builds ws URLs for local HTTP", () => {
    expect(
      buildTerminalWebSocketUrl(locationLike("http://127.0.0.1:4187/runs/run_1"), "run_1"),
    ).toBe("ws://127.0.0.1:4187/api/v1/runs/run_1/terminal");
  });

  test("builds wss URLs for HTTPS", () => {
    expect(
      buildTerminalWebSocketUrl(locationLike("https://fabro.example/runs/run/1"), "run/1"),
    ).toBe("wss://fabro.example/api/v1/runs/run%2F1/terminal");
  });

  test("parses terminal server control messages", () => {
    expect(parseTerminalServerMessage('{"type":"ready"}')).toEqual({ type: "ready" });
    expect(parseTerminalServerMessage('{"type":"error","message":"no sandbox"}')).toEqual({
      type: "error",
      message: "no sandbox",
    });
    expect(parseTerminalServerMessage('{"type":"unknown"}')).toBeNull();
    expect(parseTerminalServerMessage("{")).toBeNull();
  });

  test("reserves space above the run steering bar", () => {
    expect(TERMINAL_DOCK_CLEARANCE_CLASS).toContain("--fabro-interview-dock-clearance");
  });

  test("labels sandbox access commands by provider", () => {
    expect(terminalAccessCommandLabel("daytona")).toBe("SSH");
    expect(terminalAccessCommandLabel("docker")).toBe("Exec");
    expect(terminalAccessCommandLabel("local")).toBeNull();
    expect(terminalAccessCommandLabel(null)).toBeNull();
  });

  test("uses sandbox id as terminal status detail", () => {
    expect(sandboxStatusDetail({
      provider: "docker",
      image: null,
      snapshot: null,
      runtime: {
        id: "container-abc123",
        working_directory: "/workspace",
        repo_cloned: null,
        clone_origin_url: null,
        clone_branch: null,
      },
    }))
      .toBe("container-abc123");
    expect(sandboxStatusDetail({
      provider: "daytona",
      image: null,
      snapshot: null,
      runtime: {
        id: "sandbox-name",
        working_directory: "/workspace",
        repo_cloned: null,
        clone_origin_url: null,
        clone_branch: null,
      },
    }))
      .toBe("sandbox-name");
    expect(sandboxStatusDetail({
      provider: "docker",
      image: null,
      snapshot: null,
      runtime: null,
    })).toBe("docker");
    expect(sandboxStatusDetail(null)).toBeNull();
  });
});
