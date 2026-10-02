import type { On, RenderSurface } from "claude-code";
import { describe, expect, mock, test, tier } from "claude-code/testing";

tier("user");
function world(
  on: On,
  {
    protocol = 1,
    evaluator = false,
    hostVersion = "2.1.273",
    cards = 0,
    card = {},
    surfaces = ["terminal"],
  }: {
    protocol?: number;
    evaluator?: boolean;
    hostVersion?: string;
    cards?: number;
    card?: Record<string, unknown>;
    surfaces?: RenderSurface[];
  } = {},
) {
  const clock = mock.clock(on);
  mock.env(on, {
    Z_REPORT_DATA_DIR: "/isolated store",
    ...(evaluator ? { Z_REPORT_EVALUATOR: "1" } : {}),
  });
  on("session.start", ($, e) => ({ cwd: e.cwd }));
  const roster = [...surfaces];
  on("session.surfaces", () => ({ value: roster }));
  on("session.attach", ($, e) => {
    roster.push(e.surface);
    return { clientId: e.clientId };
  });
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
  const closed: string[] = [];
  on("ui.close", ($, e) => {
    closed.push(e.id);
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
    if (cards && input.action === "candidates")
      data = Array.from({ length: cards }, (_, i) => ({
        id: i === 0 ? "card" : `card-${i + 1}`,
        revision: 7,
        day: "2026-09-15",
        title: i === 0 ? "Fixture" : `Fixture ${i + 1}`,
        contribution: "Recorded work",
        outcomes: [],
        uncertainties: [],
        agents: ["claude"],
        pr_links: [],
        session_ids: ["session"],
        evidence_level: 2,
        status: "pending",
        related: null,
        repo: "/work/org/repo",
        ...(i === 0 ? card : {}),
      }));
    if (input.action === "evidence")
      data = [
        {
          session_id: "session",
          title: "Fixture session",
          agent: "claude",
          cli_version: "2.1.273",
          first_ts: "2026-09-15T08:05:00.000Z",
          last_ts: "2026-09-15T09:40:00.000Z",
          git_branch: "main",
          commands: [{}, {}, {}],
          commits: [{}],
          files_changed: [{}, {}],
          pr_links: [
            {
              number: 12,
              repository: "org/repo",
              ts: null,
              url: "https://x/12",
            },
          ],
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
  return { clock, registered, opened, closed, calls, notices };
}
const sdkStart = { cwd: "/work", surface: null, isInteractive: false };
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
type Node = {
  type?: string;
  props?: Record<string, unknown>;
  children?: unknown[];
};
const find = (node: unknown, test: (n: Node) => boolean): Node | undefined => {
  if (!node || typeof node !== "object") return undefined;
  const n = node as Node;
  if (test(n)) return n;
  for (const child of n.children ?? []) {
    const hit = find(child, test);
    if (hit) return hit;
  }
  return undefined;
};
const byKey = (key: string) => (n: Node) => n.props?.key === key;
const texts = (node: unknown, out: string[] = []): string[] => {
  if (typeof node === "string") out.push(node);
  else if (node && typeof node === "object")
    for (const child of (node as Node).children ?? []) texts(child, out);
  return out;
};
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
    const w = world(on, { surfaces: [] });
    await $.session.start(sdkStart);
    const result = await $.command.run(command("x-read"));
    await w.clock.settle();
    expect(result.text).toContain("interactive");
    expect(w.calls).toEqual([]);
  });
  test("the desktop app opens the same pane and catches up", async ($, on) => {
    const w = world(on, { surfaces: ["desktop"] });
    await $.session.start(sdkStart);
    await w.clock.settle();
    expect(
      w.calls
        .filter((c) => c.input.action === "read_start")
        .map((c) => c.input.automatic),
    ).toEqual([true]);
    await $.command.run(command(""));
    await w.clock.settle();
    expect(w.opened).toEqual(["z-report"]);
    const tree = await $.ui.render({ ...pane, surface: "desktop" });
    expect(find(tree, byKey("tab"))).toBeDefined();
    expect(texts(tree).join(" ")).not.toContain("ctrl+x");
  });
  test("a desktop client attaching after startup enables the journal", async ($, on) => {
    const w = world(on, { surfaces: [] });
    await $.session.start(sdkStart);
    await w.clock.settle();
    expect(w.calls).toEqual([]);
    await $.session.attach({ surface: "desktop", clientId: "desktop:default" });
    await w.clock.settle();
    expect(w.calls.some((c) => c.input.action === "read_start")).toBe(true);
  });
  test("a mobile client alone never enables the journal", async ($, on) => {
    const w = world(on, { surfaces: ["mobile"] });
    await $.session.start(sdkStart);
    await $.session.attach({ surface: "mobile", clientId: "mobile:default" });
    await w.clock.settle();
    expect(w.calls).toEqual([]);
  });
  test("the evaluator gate wins over an attached desktop", async ($, on) => {
    const w = world(on, { evaluator: true, surfaces: ["desktop"] });
    await $.session.start(sdkStart);
    await $.session.attach({ surface: "desktop", clientId: "desktop:default" });
    await w.clock.settle();
    expect(w.calls).toEqual([]);
  });
  test("nested evaluator gate wins over an interactive host", async ($, on) => {
    const w = world(on, { evaluator: true });
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
    const w = world(on, { protocol: 99 });
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
    const w = world(on, { cards: 1 });
    await $.session.start({
      cwd: "/work",
      surface: "terminal",
      isInteractive: true,
    });
    await w.clock.settle();
    await $.command.run(command(""));
    await w.clock.settle();
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
  test("the candidate picker pages at the Select limit", async ($, on) => {
    const w = world(on, { cards: 65 });
    await $.session.start({
      cwd: "/work",
      surface: "terminal",
      isInteractive: true,
    });
    await w.clock.settle();
    await $.command.run(command(""));
    await w.clock.settle();
    let tree = await $.ui.render(pane);
    let select = find(tree, byKey("candidate"));
    expect((select?.props?.options as unknown[]).length).toBe(64);
    expect(find(tree, byKey("earlier"))).toBeUndefined();
    expect(find(tree, byKey("later"))).toBeDefined();
    await $.ui.press({
      plugin: "z-report",
      key: "later",
      requestId: "z-report",
    });
    await w.clock.settle();
    tree = await $.ui.render(pane);
    select = find(tree, byKey("candidate"));
    expect(select?.props?.value).toBe("card-65");
    expect((select?.props?.options as unknown[]).length).toBe(1);
    expect(find(tree, byKey("earlier"))).toBeDefined();
    expect(find(tree, byKey("later"))).toBeUndefined();
  });
  test("the card hides evidence ids, urls and open questions", async ($, on) => {
    const w = world(on, {
      cards: 1,
      card: {
        outcomes: [
          {
            claim: "Shipped the thing",
            evidence_level: 3,
            evidence_refs: ["cmd:session:4", "pr:org/repo#12"],
            verified: true,
          },
        ],
        uncertainties: ["Was it deployed?", "Did the docs change?"],
        pr_links: [
          { number: 12, repository: "org/repo", ts: "", url: "https://x/12" },
          { number: 3, repository: "org/repo", ts: "", url: "https://x/3" },
        ],
      },
    });
    await $.session.start({
      cwd: "/work",
      surface: "terminal",
      isInteractive: true,
    });
    await w.clock.settle();
    await $.command.run(command(""));
    await w.clock.settle();
    let tree = await $.ui.render(pane);
    let body = texts(tree).join("\n");
    expect(body).toContain("✓ Shipped the thing");
    expect(body).toContain("evidence 2 · repo");
    expect(body).not.toContain("/work/org/repo");
    expect(body).not.toContain("cmd:session:4");
    expect(body).toContain("PRs #12 #3");
    expect(body).not.toContain("https://x/12");
    expect(body).not.toContain("Was it deployed?");
    expect(find(tree, byKey("uncertainties"))?.props?.label).toBe(
      "Show 2 open questions",
    );
    await $.ui.press({
      plugin: "z-report",
      key: "uncertainties",
      requestId: "z-report",
    });
    await w.clock.settle();
    tree = await $.ui.render(pane);
    body = texts(tree).join("\n");
    expect(body).toContain("· Was it deployed?");
  });
  test("inspecting evidence summarises each session", async ($, on) => {
    const w = world(on, { cards: 1 });
    await $.session.start({
      cwd: "/work",
      surface: "terminal",
      isInteractive: true,
    });
    await w.clock.settle();
    await $.command.run(command(""));
    await w.clock.settle();
    await $.ui.render(pane);
    await $.ui.press({
      plugin: "z-report",
      key: "evidence",
      requestId: "z-report",
    });
    await w.clock.settle();
    const body = texts(await $.ui.render(pane)).join("\n");
    expect(body).toMatch(
      /Fixture session · claude 2\.1\.273 · \d\d:\d\d–\d\d:\d\d/,
    );
    expect(body).toContain("3 commands · 1 commit · 2 files · PRs #12 · main");
    expect(body).not.toContain("session_id");
  });
  test("escape leaves the pane open and the Close button closes it", async ($, on) => {
    const w = world(on, { cards: 1 });
    await $.session.start({
      cwd: "/work",
      surface: "terminal",
      isInteractive: true,
    });
    await w.clock.settle();
    await $.command.run(command(""));
    await w.clock.settle();
    expect(w.opened).toEqual(["z-report"]);
    await $.ui.render(pane);
    await $.ui.press({
      plugin: "z-report",
      key: "close",
      requestId: "z-report",
    });
    await w.clock.settle();
    expect(w.closed).toEqual(["z-report"]);
  });
  test("a newer Claude Code version than the tested one is accepted", async ($, on) => {
    const w = world(on, { hostVersion: "2.1.286" });
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
    const w = world(on, { hostVersion: "2.1.272" });
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
