import type { On } from "claude-code";
import { describe, expect, mock, test, tier } from "claude-code/testing";

tier("user");
function world(
  on: On,
  protocol = 1,
  evaluator = false,
  hostVersion = "2.1.273",
  withCard = false,
) {
  const clock = mock.clock(on);
  mock.env(on, {
    Z_REPORT_DATA_DIR: "/isolated store",
    ...(evaluator ? { Z_REPORT_EVALUATOR: "1" } : {}),
  });
  on("session.start", ($, e) => ({ cwd: e.cwd }));
  const registered: string[] = [],
    opened: string[] = [];
  const calls: { args: string[]; input: Record<string, unknown> }[] = [];
  on("command.register", ($, e) => {
    registered.push(e.name);
    return { value: { command: e.name } };
  });
  on("ui.open", ($, e) => {
    opened.push(e.id);
    return { value: undefined };
  });
  on("ui.invalidate", () => ({ value: undefined }));
  const notices: string[] = [];
  on("ui.log", ($, e) => {
    notices.push(JSON.stringify(e));
    return { value: undefined };
  });
  const read = {
    id: "read1",
    status: "running",
    completed_days: 0,
    total_days: 1,
    candidate_count: 1,
    message: "Evaluating",
    scan: null,
  };
  const settings = {
    auto_catchup: true,
    retain_prompts: true,
    cost_limit_enabled: true,
    excluded_repos: [],
    claude_path: null,
    retention_days: 90,
  };
  let current: typeof read | null = null;
  on("process.run", ($, e) => {
    const input =
      e.argv.includes("rpc") && e.init?.stdin ? JSON.parse(e.init.stdin) : {};
    calls.push({ args: [...e.argv], input });
    if (e.argv.includes("--version"))
      return {
        value: {
          exitCode: 0,
          stdout: `${hostVersion} (Claude Code)`,
          stderr: "",
        },
      };
    let data: unknown = null;
    if (e.argv.includes("info"))
      data = {
        engine: "z-report",
        database_version: 1,
        tested_host: "2.1.273",
      };
    if (input.action === "overview")
      data = {
        today: "2026-09-15",
        initialized: false,
        last_successful_read_at: null,
        read: current,
        settings,
        evaluators: [],
      };
    if (
      input.action === "candidates" ||
      input.action === "journal" ||
      input.action === "eval_runs"
    )
      data = [];
    if (withCard && input.action === "candidates")
      data = [
        {
          id: "card",
          revision: 7,
          day: "2026-09-15",
          title: "Fixture",
          contribution: "Recorded work",
          outcomes: [],
          uncertainties: [],
          agents: ["claude"],
          pr_links: [],
          session_ids: ["session"],
          evidence_level: 2,
          status: "pending",
          related: null,
        },
      ];
    if (input.action === "export")
      data = { markdown: "# Journal", entries: [] };
    if (input.action === "update_settings") data = settings;
    if (input.action === "settings") data = settings;
    if (input.action === "read_start" && !input.automatic) {
      current = { ...read };
      data = { read: current, owned: true };
    }
    if (input.action === "read_heartbeat") {
      current = { ...read, status: "completed", completed_days: 1 };
      data = current;
    }
    if (input.action === "read_cancel") {
      current = { ...read, status: "cancelled" };
      data = current;
    }
    return {
      value: {
        exitCode: 0,
        stdout: JSON.stringify({ protocol, ok: true, data }),
        stderr: "",
      },
    };
  });
  return { clock, registered, opened, calls, notices };
}
const command = (args: string) => ({
  command: "z-report",
  args,
  origin: { kind: "composer" as const },
  presentation: { isFullscreen: true, columns: 80 },
});
describe("Z Report", () => {
  test("startup registers the command and only checks automatic eligibility", async ($, on) => {
    const w = world(on);
    await $.session.start({
      cwd: "/work",
      surface: "terminal",
      isInteractive: true,
    });
    await w.clock.settle();
    expect(w.registered).toEqual(["z-report"]);
    expect(
      w.calls
        .filter((c) => c.input.action === "read_start")
        .map((c) => c.input.automatic),
    ).toEqual([true]);
    expect(w.opened).toEqual([]);
  });
  test("headless, SDK and evaluator sessions never call the engine", async ($, on) => {
    const w = world(on);
    await $.session.start({
      cwd: "/work",
      surface: null,
      isInteractive: false,
    });
    const result = await $.command.run(command("x-read"));
    await w.clock.settle();
    expect(result.text).toContain("interactive");
    expect(w.calls).toEqual([]);
  });
  test("nested evaluator gate wins over an interactive host", async ($, on) => {
    const w = world(on, 1, true);
    await $.session.start({
      cwd: "/work",
      surface: "terminal",
      isInteractive: true,
    });
    await w.clock.settle();
    expect(w.calls).toEqual([]);
  });
  test("manual launch uses JSON and polls to completion once", async ($, on) => {
    const w = world(on);
    await $.session.start({
      cwd: "/work",
      surface: "terminal",
      isInteractive: true,
    });
    await w.clock.settle();
    await $.command.run(command("x-read"));
    await w.clock.settle();
    const launch = w.calls.find(
      (c) => c.input.action === "read_start" && c.input.automatic === false,
    );
    expect(launch?.args.slice(1)).toEqual([
      "--data-dir",
      "/isolated store",
      "rpc",
    ]);
    await w.clock.advance(2000);
    await w.clock.settle();
    expect(w.notices.length).toBe(1);
    const count = w.calls.length;
    await w.clock.advance(10000);
    expect(w.calls.length).toBe(count);
  });
  test("unsupported arguments are rejected without additional engine calls", async ($, on) => {
    const w = world(on);
    await $.session.start({
      cwd: "/work",
      surface: "terminal",
      isInteractive: true,
    });
    await w.clock.settle();
    const count = w.calls.length;
    const result = await $.command.run(command("x-read; touch /tmp/no"));
    expect(result.text).toContain("Usage:");
    expect(w.calls.length).toBe(count);
  });
  test("native narrow pane exposes the same manual read", async ($, on) => {
    const w = world(on);
    await $.session.start({
      cwd: "/work",
      surface: "terminal",
      isInteractive: true,
    });
    await w.clock.settle();
    await $.command.run(command(""));
    await w.clock.settle();
    await $.ui.render({
      surface: "terminal",
      component: "Pane",
      requestId: "z-report",
      props: {
        title: "Z Report",
        isFocused: true,
        bodyColumns: 40,
        placement: "inline",
        scroll: { offset: 0, bodyRows: 20 },
        view: {},
      },
    });
    await $.ui.press({ plugin: "z-report", key: "run", requestId: "z-report" });
    await w.clock.settle();
    expect(
      w.calls.filter(
        (c) => c.input.action === "read_start" && !c.input.automatic,
      ).length,
    ).toBe(1);
  });
  test("incompatible engines never access the database", async ($, on) => {
    const w = world(on, 99);
    await $.session.start({
      cwd: "/work",
      surface: "terminal",
      isInteractive: true,
    });
    await w.clock.settle();
    await $.command.run(command("x-read"));
    await w.clock.settle();
    expect(
      w.calls.every(
        (c) => c.args.includes("info") || c.args.includes("--version"),
      ),
    ).toBe(true);
  });
  test("review saves and approval preserve the revision", async ($, on) => {
    const w = world(on, 1, false, "2.1.273", true);
    await $.session.start({
      cwd: "/work",
      surface: "terminal",
      isInteractive: true,
    });
    await w.clock.settle();
    await $.command.run(command(""));
    await w.clock.settle();
    const pane = {
      surface: "terminal" as const,
      component: "Pane" as const,
      requestId: "z-report",
      props: {
        title: "Z Report",
        isFocused: true,
        bodyColumns: 40,
        placement: "inline" as const,
        scroll: { offset: 0, bodyRows: 20 },
        view: {},
      },
    };
    await $.ui.render(pane);
    await $.ui.press({
      plugin: "z-report",
      key: "edit",
      requestId: "z-report",
    });
    await w.clock.settle();
    await $.ui.render(pane);
    await $.ui.press({
      plugin: "z-report",
      key: "save-edit",
      requestId: "z-report",
    });
    await w.clock.settle();
    const edit = w.calls.find(
      (c) => c.input.action === "edit_candidate",
    )?.input;
    expect(edit?.title).toBe("Fixture");
    expect(edit?.revision).toBe(7);
    await $.ui.render(pane);
    await $.ui.press({
      plugin: "z-report",
      key: "approve",
      requestId: "z-report",
    });
    await w.clock.settle();
    expect(
      w.calls.find((c) => c.input.action === "approve_candidate")?.input
        .revision,
    ).toBe(7);
  });
  test("a newer Claude Code version than the tested one is accepted", async ($, on) => {
    const w = world(on, 1, false, "2.1.286");
    await $.session.start({
      cwd: "/work",
      surface: "terminal",
      isInteractive: true,
    });
    await w.clock.settle();
    await $.command.run(command("x-read"));
    await w.clock.settle();
    expect(w.calls.some((c) => c.input.action === "read_start")).toBe(true);
  });
  test("an older Claude Code version than the tested one cannot access the database", async ($, on) => {
    const w = world(on, 1, false, "2.1.272");
    await $.session.start({
      cwd: "/work",
      surface: "terminal",
      isInteractive: true,
    });
    await w.clock.settle();
    await $.command.run(command("x-read"));
    await w.clock.settle();
    expect(
      w.calls.every(
        (c) => c.args.includes("--version") || c.args.includes("info"),
      ),
    ).toBe(true);
  });
});
