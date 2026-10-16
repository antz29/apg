import { Plugin } from "@opencode/plugin"
import { runCli, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_plan_add",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_plan_add",
        description:
          "Plan mutations (`apg plan <action> …`) — the strict add/update/rm surface (no implicit upsert). `action` defaults to \"add\". Plan record: `add <project> [--title T] [--strategy S]` creates the plan (refuses when it exists), `update <project> [--title T] [--strategy S]` MERGEs an existing plan, `rm <project> [--force]` removes it (refuses while it has phases unless `force`). Sub-entities via `kind`: `phase` (number), `task` (phaseNumber + taskNumber), `planned` (fqn). `add` authors a new entity (refuses an existing one); `update` edits in place and is edge-preserving (a phase update keeps its tasks, a task update keeps status + Reviews, a planned update repoints the parent Contains) — `update phase` also sets/replaces the phase's Satisfies and Gates sets via satisfies/prereq; `rm` deletes (refuses while dependents exist unless `force`). Phase: deliverable, prereq (phase numbers), satisfies (requirement NAMES from the layers store — `requirements.requirement.<name>`). Task: taskKind (source/test/gate/docs — owning role), tier (unit/int/e2e — required for test tasks only), verb (creates/modifies/deletes/renames/moves — the Task→Implementation verb), fqn (the Implementation FQN the verb applies to; for renames/moves the source FQN), to (the new FQN for renames/moves). On a task update an omitted fqn leaves the target unchanged, while an explicitly empty fqn (fqn: \"\") clears the target to the target-less form — forwarded as `--no-fqn`, legal only with verb=creates; the CLI refuses `--fqn` together with `--no-fqn`. Planned node: plannedKind (module/file/struct/function), fqn (the real code FQN where the code will land), optional name + parent. Mutations run guarded inside a project worktree on the project branch.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            action: { type: "string", description: '"add" (default), "update", or "rm".' },
            project: { type: "string", description: "Plan project (required)." },
            kind: {
              type: "string",
              description: 'Optional sub-entity: "phase", "task", or "planned". Omit it for the plan record itself.',
            },
            number: { type: "string", description: "For phase: the phase number." },
            phaseNumber: { type: "string", description: "For task: the phase number the task belongs to." },
            taskNumber: { type: "string", description: "For task: the task number within the phase." },
            title: { type: "string", description: "Phase or task title (required for add)." },
            strategy: {
              type: "string",
              description: "For the plan record (add/update): strategy text (variants, tier routing, gates, execution method).",
            },
            deliverable: { type: "string", description: "For phase: one-line deliverable." },
            prereq: {
              type: "array",
              items: { type: "string" },
              description:
                "For phase: phase numbers this phase is gated on (prereqs). On update, passed values replace the phase's Gates set.",
            },
            satisfies: {
              type: "array",
              items: { type: "string" },
              description:
                "For phase: requirement names this phase delivers (Satisfies → requirements.requirement.<name>). On update, passed values replace the phase's Satisfies set.",
            },
            taskKind: {
              type: "string",
              description: "For task: the owning role — source (default), test, gate, or docs.",
            },
            tier: {
              type: "string",
              description: "For task: verification depth — unit, int, or e2e. Only valid with taskKind=test (required then).",
            },
            verb: {
              type: "string",
              description:
                "For task: the Task→Implementation verb — creates, modifies, deletes, renames, or moves (default creates).",
            },
            fqn: {
              type: "string",
              description:
                'For task: the Implementation FQN the verb applies to (for renames/moves, the source FQN). On a task update, an explicitly empty string ("") clears the target to the target-less form (only with verb=creates), while omitting it leaves the target unchanged; the CLI refuses --fqn together with --no-fqn. For planned node: the real code FQN where the code will land (required).',
            },
            to: { type: "string", description: "For task renames/moves: the destination FQN." },
            plannedKind: {
              type: "string",
              description: "For planned node: module, file, struct, or function (required for add).",
            },
            name: { type: "string", description: "For planned node: the simple name." },
            parent: { type: "string", description: "For planned node: the containing node FQN." },
            force: {
              type: "boolean",
              description:
                "For rm: cascade removal of the entity and its dependents (plan-with-phases, phase-with-tasks, done/feedback task, creates-targeted planned).",
            },
          },
          required: ["project"],
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
          const { project, kind } = input
          if (!project) return { content: "Error: project is required" }
          const action = input.action ?? "add"
          if (action !== "add" && action !== "update" && action !== "rm") {
            return { content: 'Error: action must be "add", "update", or "rm"' }
          }
          if (kind && kind !== "phase" && kind !== "task" && kind !== "planned") {
            return { content: 'Error: kind must be "phase", "task", or "planned"' }
          }

          const cli = ["plan", action, project]

          if (action === "rm") {
            if (kind === "phase") {
              if (!input.number) return { content: "Error: rm phase requires number" }
              cli.push("phase", input.number)
            } else if (kind === "task") {
              if (!input.phaseNumber || !input.taskNumber)
                return { content: "Error: rm task requires phaseNumber and taskNumber" }
              cli.push("task", input.phaseNumber, input.taskNumber)
            } else if (kind === "planned") {
              if (!input.fqn) return { content: "Error: rm planned requires fqn" }
              cli.push("planned", input.fqn)
            }
            if (input.force) cli.push("--force")
            return { content: await runCli(merged, cli, input.directory) }
          }

          if (action === "update") {
            if (kind === "phase") {
              if (!input.number) return { content: "Error: update phase requires number" }
              cli.push("phase", input.number)
              if (input.title) cli.push("--title", input.title)
              if (input.deliverable) cli.push("--deliverable", input.deliverable)
              for (const p of input.prereq ?? []) cli.push("--prereq", p)
              for (const s of input.satisfies ?? []) cli.push("--satisfies", s)
            } else if (kind === "task") {
              if (!input.phaseNumber || !input.taskNumber)
                return { content: "Error: update task requires phaseNumber and taskNumber" }
              cli.push("task", input.phaseNumber, input.taskNumber)
              if (input.title) cli.push("--title", input.title)
              if (input.taskKind) cli.push("--kind", input.taskKind)
              if (input.tier) cli.push("--tier", input.tier)
              if (input.verb) cli.push("--verb", input.verb)
              if (input.fqn === "") cli.push("--no-fqn")
              else if (input.fqn) cli.push("--fqn", input.fqn)
              if (input.to) cli.push("--to", input.to)
            } else if (kind === "planned") {
              if (!input.fqn) return { content: "Error: update planned requires fqn" }
              cli.push("planned", input.fqn)
              if (input.plannedKind) cli.push("--kind", input.plannedKind)
              if (input.name) cli.push("--name", input.name)
              if (input.parent) cli.push("--parent", input.parent)
            } else {
              if (input.title) cli.push("--title", input.title)
              if (input.strategy) cli.push("--strategy", input.strategy)
            }
            return { content: await runCli(merged, cli, input.directory) }
          }

          // action === "add"
          if (kind === "phase") {
            if (!input.number) return { content: "Error: phase requires number" }
            if (!input.title) return { content: "Error: phase requires title" }
            cli.push("phase", input.number, "--title", input.title)
            if (input.deliverable) cli.push("--deliverable", input.deliverable)
            for (const p of input.prereq ?? []) cli.push("--prereq", p)
            for (const s of input.satisfies ?? []) cli.push("--satisfies", s)
          } else if (kind === "task") {
            if (!input.phaseNumber || !input.taskNumber)
              return { content: "Error: task requires phaseNumber and taskNumber" }
            if (!input.title) return { content: "Error: task requires title" }
            cli.push("task", input.phaseNumber, input.taskNumber, "--title", input.title)
            if (input.taskKind) cli.push("--kind", input.taskKind)
            if (input.tier) cli.push("--tier", input.tier)
            if (input.verb) cli.push("--verb", input.verb)
            if (input.fqn) cli.push("--fqn", input.fqn)
            if (input.to) cli.push("--to", input.to)
          } else if (kind === "planned") {
            if (!input.plannedKind || !input.fqn)
              return { content: "Error: planned node requires plannedKind and fqn" }
            cli.push("planned", input.plannedKind, input.fqn)
            if (input.name) cli.push("--name", input.name)
            if (input.parent) cli.push("--parent", input.parent)
          } else {
            if (input.title) cli.push("--title", input.title)
            if (input.strategy) cli.push("--strategy", input.strategy)
          }
          return { content: await runCli(merged, cli, input.directory) }
        },
      })
    })
  },
})
