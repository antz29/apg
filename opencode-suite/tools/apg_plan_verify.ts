import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Run the pre-merge coherence gate: `apg plan verify <project>` (renamed from `apply` — the binary applies nothing). Verifies every planned Implementation node in the branch is realized (a scan found real code at its FQN), all Feedback is resolved, and derived solution coverage holds. On green it prints the merge handoff. Read-only; the merge itself is `apg project merge <project>` from the main checkout.",
  args: {
    project: tool.schema.string().describe("Plan project (required)."),
  },
  async execute(args, context) {
    const { project } = args
    if (!project) return "Error: project is required"
    return runCli(context, ["plan", "verify", project])
  },
})
