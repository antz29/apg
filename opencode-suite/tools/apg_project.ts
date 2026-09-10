import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Project lifecycle. `start <name>` — run from the MAIN checkout: creates the project worktree at <main>/apg/.worktrees/<name>, a branch off the repo's default branch, and auto-scans it (worktree + branch + branch DB in one command); the binary prints the worktree path and sessions then operate with cwd INSIDE that worktree (walk-up discovery finds the worktree's own apg/). `verify <name>` — the pre-merge coherence gate (`apg plan verify`): every planned node realized, all feedback resolved, derived solution coverage holds; read-only. `merge <name>` — run from the MAIN checkout: verify gate → merge the project branch → rebuild main's graph unguarded. Main is never a mutation place.",
  args: {
    action: tool.schema.string().describe('"start", "verify", or "merge" (required).'),
    name: tool.schema.string().describe("Project name (== branch name == worktree dir) (required)."),
  },
  async execute(args, context) {
    const { action, name } = args
    if (!action || !name) return "Error: action and name are required"
    if (action === "start" || action === "merge") {
      return runCli(context, ["project", action, name])
    }
    if (action === "verify") {
      return runCli(context, ["plan", "verify", name])
    }
    return 'Error: action must be "start", "verify", or "merge"'
  },
})
