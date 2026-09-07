import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Run the apply-act coherence gate: `apg plan apply <project>`. Verifies every Builds(Task→Future) target resolves in the branch's graph and all Feedback is resolved (PlanCompletion-SPEC.md). On green it prints the merge + rebuild handoff — the navigator then operates `git merge <project>` into main and rebuilds main's graph. This tool performs NO merge and NO mutation; it is the pre-merge gate check only.",
  args: {
    project: tool.schema.string().describe("Plan project (required)."),
  },
  async execute(args, context) {
    const { project } = args
    if (!project) return "Error: project is required"
    return runCli(context, ["plan", "apply", project])
  },
})