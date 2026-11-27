import { Plugin } from "@opencode/plugin"
import { apgBinary, resolveProjectWorktree, type ToolContext } from "../lib/apg.ts"

type ApgSpecLintInput = { project: string; tier?: "requirements" | "domain" | "solution" }

export default Plugin.define({
  id: "apg_spec_lint",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_spec_lint",
        options: { codemode: false },
        description:
          "Run the deterministic, read-only durable-spec lint (`apg spec lint`) in the registered project's resolved worktree and return its report. Reports the shared mechanical validator set (the edge matrix, the event-only publishes/subscribes target rule, and the whole-model cardinalities the write path also uses), the R3/R4 rule violations, and the tier-1-3 body budget as ERRORS (a non-zero exit) and the tier-1-3 wording advisories as non-blocking warnings — the deterministic pre-pass the plan-review and spec-review passes run before their semantic review. Whole-model change-set delta coverage is excluded (a plan-verification concern). Reads the durable node files; never writes. `--tier requirements|domain|solution` selects a cumulative scope: the named tier is checked together with every tier above it in the catalog ladder (requirements above domain above solution), including each tier-crossing coverage rule whose supplying tier is in scope (requirement coverage needs Domain, service realisation needs Solution). Omit `tier` to lint all tiers. `lint` is the only subcommand.",
        input: {
          type: "object",
          properties: {
            project: {
              type: "string",
              description: "The registered project name (as recorded by `apg project start` / `apg init`). The tool resolves it to that project's worktree.",
            },
            tier: {
              type: "string",
              enum: ["requirements", "domain", "solution"],
              description: "Optional cumulative lint scope: the named tier is checked together with every tier above it in the catalog ladder (requirements above domain above solution), including each tier-crossing coverage rule whose supplying tier is in scope (requirement coverage needs Domain, service realisation needs Solution). Omit to lint all tiers.",
            },
          },
          required: ["project"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgSpecLintInput
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          merged.projectName = input.project
          merged.worktree = await resolveProjectWorktree(merged, input.project)
          // `apg spec lint` prints its whole report to stderr (the blocking errors AND
          // the non-blocking advisories) and only a one-line summary to stdout, so the
          // shared `runCli` (whose success contract is stdout-only) would drop the
          // advisories on a clean run. Capture both streams, like `apg_scan`.
          const args = [apgBinary(), "spec", "lint"]
          if (input.tier) args.push("--tier", input.tier)
          const proc = Bun.spawn(args, {
            cwd: merged.worktree,
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
