import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Archive a fully-implemented spec: `apg spec archive <project>`. Refuses while any Feedback on the spec's nodes is open/actioned (every review item must be resolved first). Moves apg/specs/<project>.jsonl to apg/archived/ so it leaves active discovery; Implements edges keep delivered work traceable. No FQN changes.",
  args: {
    project: tool.schema.string().describe("Spec project to archive (required)."),
  },
  async execute(args, context) {
    const { project } = args
    if (!project) return "Error: project is required"
    return runCli(context, ["spec", "archive", project])
  },
})