import { Plugin } from "@opencode/plugin"
import { mainCheckoutRoot, resolveProjectWorktree, runCli, runInProject, type ToolContext } from "../lib/apg.ts"

type ApgProjectInput = { project: string; action: "start" | "verify" | "merge" | "claim" }

export default Plugin.define({
  id: "apg_project",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_project",
        options: { codemode: false },
        description:
          "Project lifecycle. `start <project>` — run from the MAIN checkout: creates the project worktree at <main>/apg/.worktrees/<project>, a branch off the repo's default branch, and seeds the branch DB by copying the main checkout's current scan (run `apg scan` on main first — start refuses a stale or missing scan); the binary prints the worktree path, and suite tools take a registered project name, resolve it to that worktree, and run with it as cwd — so sessions operate with cwd inside it. `verify <project>` — the pre-merge coherence gate (`apg plan verify`): every planned node realized, all feedback resolved, derived solution coverage holds; read-only. `merge <project>` — run from the MAIN checkout: verify gate → merge the project branch → rebuild main's graph unguarded. `claim <project>` — take ownership of an ownerless registered project (e.g. one recorded by `apg project reconstruct`): replaces the single owner with the calling session, reports the previous owner, and refuses `main` and an unregistered name. Main is never a mutation place.",
        input: {
          type: "object",
          properties: {
            project: {
              type: "string",
              description: "The registered project name (as recorded by `apg project start` / `apg init`). The tool resolves it to that project's worktree.",
            },
            action: { type: "string", description: '"start", "verify", "merge", or "claim" (required).' },
          },
          required: ["project", "action"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgProjectInput
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          const { action, project } = input
          if (!action || !project) return { content: "Error: action and project are required" }
          // verify/claim act on the project's own row: resolve it to its worktree
          // and run there with the caller's project environment set.
          if (action === "verify" || action === "claim") {
            merged.projectName = project
            merged.worktree = await resolveProjectWorktree(merged, project)
            const cli = action === "verify" ? ["plan", "verify", project] : ["project", "claim", project]
            return { content: await runInProject(merged, cli) }
          }
          // start/merge are main-checkout operations and need no registry row
          // (start creates it): run from the main root, never the caller's cwd.
          if (action === "start" || action === "merge") {
            const main = await mainCheckoutRoot(merged)
            if (!main) {
              return {
                content:
                  "Error: no main checkout root resolved from the caller's context. Run `apg init` in the main checkout, then `apg project start <project>` from there.",
              }
            }
            return { content: await runCli(merged, ["project", action, project], main.root) }
          }
          return { content: 'Error: action must be "start", "verify", "merge", or "claim"' }
        },
      })
    })
  },
})
