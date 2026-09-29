import { tool } from "@opencode-ai/plugin"
import { apgBinary, findApgRoot, NO_DB_ERROR } from "../lib/apg.ts"

export default tool({
  description:
    "Run the deterministic, read-only durable-spec lint (`apg spec lint`) from the project root and return its report. Reports the mechanical R2/R3/R4 rule violations and the change-set delta gates as ERRORS (a non-zero exit) and the tier-1-3 wording advisories as non-blocking warnings — the deterministic pre-pass the plan-review and spec-review passes run before their semantic review. Reads the durable node files, the merge-base git tree, and the transient plan store; never writes. `lint` is the only subcommand.",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory (a worktree path to operate on). Defaults to the workspace root."),
  },
  async execute(args, context) {
    const root = findApgRoot(context, args.directory)
    if (!root) return NO_DB_ERROR
    // `apg spec lint` prints its whole report to stderr (the blocking errors AND
    // the non-blocking advisories) and only a one-line summary to stdout, so the
    // shared `runCli` (whose success contract is stdout-only) would drop the
    // advisories on a clean run. Capture both streams, like `apg_scan`.
    const proc = Bun.spawn([apgBinary(), "spec", "lint"], {
      cwd: root,
      stdout: "pipe",
      stderr: "pipe",
    })
    const [stdout, stderr] = await Promise.all([
      new Response(proc.stdout).text(),
      new Response(proc.stderr).text(),
    ])
    const exitCode = await proc.exited
    const report = [stdout.trim(), stderr.trim()].filter((s) => s.length > 0).join("\n")
    if (exitCode !== 0) {
      return `apg spec lint found violations (exit ${exitCode}):\n${report}`
    }
    return report || "spec lint: no violations"
  },
})
