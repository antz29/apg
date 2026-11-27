import { Plugin } from "@opencode/plugin"
import { apgBinary, resolveProjectWorktree, type ToolContext } from "../lib/apg.ts"

type ApgScanInput = {
  project: string
  language?: string
  dialect?: string
  blacklist?: string
  excludePath?: string
  modules?: string
  force?: boolean
}

export default Plugin.define({
  id: "apg_scan",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_scan",
        options: { codemode: false },
        description:
          "Rebuild the registered project's LadybugDB graph database (its worktree's apg/.trans/db.lbug, derived from the durable apg/layers/** node files and the scanned code) by running the apg scanner + ingestor pipeline in that worktree; the scan also writes apg/.trans/apg-frontend.log. Run this when source files have changed and the graph is stale. Pass `force` for a from-scratch rebuild: it discards the derived database and re-runs every frontend instead of reusing the shared fact store, leaving apg/layers and source files untouched.",
        input: {
          type: "object",
          properties: {
            project: {
              type: "string",
              description: "The registered project name (as recorded by `apg project start` / `apg init`). The tool resolves it to that project's worktree and scans there.",
            },
            language: {
              type: "string",
              description: "Language(s) to scan: go, java, cpp, rust, ts, js, csharp, py, cobol, sql, ps, agent, plus the structural stream ids md, sh, yaml, json, toml, xml, dockerfile, makefile, ini, sln, misc — each must be an installed frontend. Comma-separated for a multi-language repo (e.g. \"go,ts\"); auto-detected for every language present if omitted.",
            },
            dialect: {
              type: "string",
              description: "COBOL dialect passed as --std: cobol85, cobol2002, cobol2014, or ibm. Only meaningful when scanning cobol.",
            },
            blacklist: {
              type: "string",
              description: "Comma-separated list of FQN prefixes to exclude from the graph.",
            },
            excludePath: {
              type: "string",
              description: "Comma-separated glob patterns for paths to exclude.",
            },
            modules: {
              type: "string",
              description:
                "Comma-separated `--module` dirs, each passed as its own `--module` flag and restricting the scan to that directory and its subtree. A relative value resolves against the scan root; an absolute value passes through unchanged. Honoured by every frontend apg ships — the code languages, the agent-definition frontend and every bundled structural stream — with one TypeScript granularity exception: its arm selects whole npm packages, so a value at or above a package root selects that package's subtree, while a value inside a package (e.g. packages/app/src) selects nothing. Defaults to auto-discovery of the whole tree.",
            },
            force: {
              type: "boolean",
              description:
                "Force a from-scratch rebuild: discard apg/.trans/db.lbug (with its .wal/.shm sidecars), and re-run every frontend rather than reusing the shared fact store. apg/layers and source files are left untouched.",
            },
          },
          required: ["project"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgScanInput
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          merged.projectName = input.project
          const worktree = await resolveProjectWorktree(merged, input.project)
          merged.worktree = worktree

          const spawnArgs: string[] = [apgBinary(), "scan", worktree]

          if (input.force) {
            spawnArgs.push("--force")
          }
          if (input.language) {
            spawnArgs.push("--language", input.language)
          }
          if (input.dialect) {
            spawnArgs.push("--std", input.dialect)
          }
          for (const pat of (input.excludePath ?? "").split(",").filter(Boolean)) {
            spawnArgs.push("--exclude-path", pat)
          }
          for (const m of (input.modules ?? "").split(",").filter(Boolean)) {
            spawnArgs.push("--module", m)
          }
          for (const prefix of (input.blacklist ?? "").split(",").filter(Boolean)) {
            spawnArgs.push(prefix)
          }

          const proc = Bun.spawn(spawnArgs, {
            stdout: "pipe",
            stderr: "pipe",
            cwd: worktree,
          })
          const [output, stderr] = await Promise.all([
            new Response(proc.stdout).text(),
            new Response(proc.stderr).text(),
          ])
          const exitCode = await proc.exited

          if (exitCode !== 0) {
            return { content: `Scan failed (exit code ${exitCode}):\n${stderr.slice(-4000)}\n${output.slice(-2000)}` }
          }

          // stderr carries progress lines and the graph/cleanup counts. Surface the
          // summary lines only, not the per-file progress noise.
          const summary = stderr
            .split("\n")
            .map((l) => l.replace(/\r/g, "").trim())
            .filter((l) => /^(graph:|cleanup:|WARNING:|Skipped|Project:|Language:|\[load\])/.test(l))
            .join("\n")
          return { content: summary || "Scan completed successfully." }
        },
      })
    })
  },
})
