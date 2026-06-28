import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { execFile } from "node:child_process";

const MAX_OUTPUT_CHARS = 40_000;

type LdgrResult = {
  code: number | null;
  stdout: string;
  stderr: string;
};

function parseArgs(input: string): string[] {
  const args: string[] = [];
  let current = "";
  let quote: '"' | "'" | undefined;
  let escaping = false;

  for (const char of input) {
    if (escaping) {
      current += char;
      escaping = false;
      continue;
    }
    if (char === "\\" && quote !== "'") {
      escaping = true;
      continue;
    }
    if ((char === '"' || char === "'") && !quote) {
      quote = char;
      continue;
    }
    if (char === quote) {
      quote = undefined;
      continue;
    }
    if (/\s/.test(char) && !quote) {
      if (current.length > 0) {
        args.push(current);
        current = "";
      }
      continue;
    }
    current += char;
  }

  if (escaping) current += "\\";
  if (quote) throw new Error(`unterminated ${quote} quote`);
  if (current.length > 0) args.push(current);
  return args;
}

function truncate(text: string): string {
  if (text.length <= MAX_OUTPUT_CHARS) return text;
  return `${text.slice(0, MAX_OUTPUT_CHARS)}\n\n[output truncated at ${MAX_OUTPUT_CHARS} characters]`;
}

function runLdgr(cwd: string, args: string[]): Promise<LdgrResult> {
  return new Promise((resolve, reject) => {
    execFile("ldgr", args, { cwd, timeout: 60_000 }, (error: any, stdout, stderr) => {
      if (error && error.killed) {
        reject(new Error(`ldgr ${args.join(" ")} timed out`));
        return;
      }
      resolve({
        code: error?.code ?? 0,
        stdout: String(stdout ?? "").trimEnd(),
        stderr: String(stderr ?? "").trimEnd(),
      });
    });
  });
}

function renderLdgrMessage(args: string[], result: LdgrResult): string {
  const command = `ldgr ${args.join(" ")}`.trimEnd();
  const parts = [
    `LDGR command output from \`${command}\` (exit ${result.code ?? "signal"}):`,
  ];
  if (result.stdout) parts.push(`stdout:\n\n\`\`\`text\n${truncate(result.stdout)}\n\`\`\``);
  if (result.stderr) parts.push(`stderr:\n\n\`\`\`text\n${truncate(result.stderr)}\n\`\`\``);
  if (!result.stdout && !result.stderr) parts.push("<no output>");
  return parts.join("\n\n");
}

export default function ldgrContext(pi: ExtensionAPI) {
  pi.registerCommand("ldgr", {
    description: "Run `ldgr ...` in the project and pipe stdout/stderr into the conversation.",
    handler: async (argsText, ctx) => {
      try {
        const args = parseArgs(argsText.trim());
        if (args.length === 0) args.push("context", "--brief");
        const result = await runLdgr(ctx.cwd, args);
        if (ctx.hasUI) ctx.ui.notify(`ldgr ${args.join(" ")} exited ${result.code ?? "signal"}`, result.code === 0 ? "info" : "warning");
        pi.sendUserMessage(renderLdgrMessage(args, result));
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        if (ctx.hasUI) ctx.ui.notify(`/ldgr failed: ${message}`, "warning");
      }
    },
  });

  pi.registerCommand("ldgr-context", {
    description: "Inject current `ldgr context --brief` output into the conversation.",
    handler: async (_args, ctx) => {
      try {
        const result = await runLdgr(ctx.cwd, ["context", "--brief"]);
        if (ctx.hasUI) ctx.ui.notify("ldgr-context captured current LDGR brief context", result.code === 0 ? "info" : "warning");
        pi.sendUserMessage(renderLdgrMessage(["context", "--brief"], result));
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        if (ctx.hasUI) ctx.ui.notify(`ldgr-context failed: ${message}`, "warning");
      }
    },
  });
}
