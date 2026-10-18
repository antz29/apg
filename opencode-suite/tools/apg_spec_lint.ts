import { Plugin } from "@opencode/plugin"
import { apgBinary, findApgRoot, NO_DB_ERROR, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_spec_lint",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_spec_lint",
        options: { codemode: false },
        description:
          "Run the deterministic, read-only durable-spec lint (`apg spec lint`) from the project root and return its report. Reports the shared mechanical validator set (the edge matrix, the event-only publishes/subscribes target rule, and the whole-model cardinalities the write path also uses), the R3/R4 rule violations, and the tier-1-3 body budget as ERRORS (a non-zero exit) and the tier-1-3 wording advisories as non-blocking warnings — the deterministic pre-pass the plan-review and spec-review passes run before their semantic review. Whole-model change-set delta coverage is excluded (a plan-verification concern). Reads the durable node files; never writes. Omit `tier` to lint all tiers; `--tier requirements|domain|solution` scopes the run to one tier plus the typed upstream spine paths it requires. `lint` is the only subcommand.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            tier: {
              type: "string",
              enum: ["requirements", "domain", "solution"],
              description: "Optional single-tier lint scope: validate that tier's local structure, cardinalities, and constraints plus only the typed upstream spine paths it requires. Omit to lint all tiers.",
            },
          },
          required: [],
          additionalProperties: false,
        },
        async execute(input, context) {
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          const root = findApgRoot(merged, input.directory)
          if (!root) return { content: NO_DB_ERROR }
          // `apg spec lint` prints its whole report to stderr (the blocking errors AND
          // the non-blocking advisories) and only a one-line summary to stdout, so the
          // shared `runCli` (whose success contract is stdout-only) would drop the
          // advisories on a clean run. Capture both streams, like `apg_scan`.
          const args = [apgBinary(), "spec", "lint"]
          if (input.tier) args.push("--tier", input.tier)
          const proc = Bun.spawn(args, {
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
            return { content: `apg spec lint found violations (exit ${exitCode}):\n${report}` }
          }
          return { content: report || "spec lint: no violations" }
        },
      })
    })
  },
})
