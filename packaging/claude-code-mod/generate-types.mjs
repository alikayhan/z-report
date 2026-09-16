import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const root = new URL("../../", import.meta.url);
const version = JSON.parse(readFileSync(new URL("package.json", root), "utf8"))
  .devDependencies["@anthropic-ai/claude-code"];
const workspace = mkdtempSync(join(tmpdir(), "z-report-types-"));

try {
  execFileSync("claude", [
    "--safe-mode", "--print", "--setting-sources", "",
    "--strict-mcp-config", "--no-session-persistence", "/plugin-types types",
  ], {
    cwd: workspace,
    env: {
      ...process.env,
      CLAUDE_CONFIG_DIR: join(workspace, "config"),
      CLAUDE_CODE_ENABLE_FUNCTION_HOOKS: "1",
      DISABLE_AUTOUPDATER: "1",
      DISABLE_TELEMETRY: "1",
      DISABLE_ERROR_REPORTING: "1",
    },
    stdio: ["ignore", "pipe", "inherit"],
    timeout: 30_000,
  });
  const declarations = readFileSync(join(workspace, "types/claude-code.d.ts"), "utf8");
  if (!declarations.startsWith(`// Written by Claude Code ${version}.\n`)) {
    throw new Error(`Type generation requires Claude Code ${version}; run npm ci.`);
  }
  const types = new URL("mods/claude-code/types/", root);
  mkdirSync(types, { recursive: true });
  writeFileSync(new URL("claude-code.d.ts", types), declarations);
} finally {
  rmSync(workspace, { recursive: true, force: true });
}
