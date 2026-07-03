import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { execFile } from "node:child_process";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const MAX_OUTPUT_CHARS = 40_000;
const LOOP_TIMEOUT_MS = 12 * 60 * 60 * 1000;

type LdgrResult = {
  code: number | null;
  stdout: string;
  stderr: string;
};

type AdapterRegistry = {
  adapters?: Adapter[];
};

type Adapter = {
  slug: string;
  title?: string;
  aliases?: string[];
  root_path: string;
  profile?: {
    loop_prompt_path?: string;
  };
};

type LoopChoice = {
  slug: string;
  title?: string;
  aliases?: string[];
  promptPath: string;
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

function runLdgr(cwd: string, args: string[], timeout = 60_000): Promise<LdgrResult> {
  return new Promise((resolve, reject) => {
    execFile("ldgr", args, { cwd, timeout, maxBuffer: 20 * 1024 * 1024 }, (error: any, stdout, stderr) => {
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

async function discoverLoopChoices(cwd: string): Promise<LoopChoice[]> {
  const choices: LoopChoice[] = [];
  const corePrompt = join(homedir(), ".ldgr", "prompts", "ldgr-core-loop.md");
  if (existsSync(corePrompt)) {
    choices.push({
      slug: "core",
      title: "LDGR core loop",
      aliases: ["ldgr", "ldgr-core"],
      promptPath: corePrompt,
    });
  }

  const result = await runLdgr(cwd, ["adapter", "list", "--json"]);
  if (result.code !== 0) {
    throw new Error(renderLdgrMessage(["adapter", "list", "--json"], result));
  }
  const registry = JSON.parse(result.stdout || "{}") as AdapterRegistry;
  for (const adapter of registry.adapters ?? []) {
    if (!adapter.profile?.loop_prompt_path) continue;
    choices.push({
      slug: adapter.slug,
      title: adapter.title,
      aliases: adapter.aliases,
      promptPath: join(adapter.root_path, adapter.profile.loop_prompt_path),
    });
  }
  return choices;
}

function loopChoiceMatches(choice: LoopChoice, token: string): boolean {
  return choice.slug === token || (choice.aliases ?? []).includes(token);
}

function renderRunLoopMessage(choice: LoopChoice, commandArgs: string[], result: LdgrResult): string {
  return renderLdgrMessage(commandArgs, result)
    + `\n\nLoop selected: ${choice.slug}${choice.title ? ` (${choice.title})` : ""}`;
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
  pi.registerCommand("run-loop", {
    description: "Select the LDGR core or installed adapter loop and run it through agentctl.",
    handler: async (argsText, ctx) => {
      try {
        const tokens = parseArgs(argsText.trim());
        const choices = await discoverLoopChoices(ctx.cwd);
        if (choices.length === 0) {
          if (ctx.hasUI) ctx.ui.notify("No LDGR core loop prompt or installed adapter loop prompts were discovered. Run `ldgr install` or install an adapter with a loop prompt.", "warning");
          return;
        }

        let selected: LoopChoice | undefined;
        if (tokens.length > 0 && !tokens[0].startsWith("-")) {
          const requested = choices.find((choice) => loopChoiceMatches(choice, tokens[0]));
          if (!requested) {
            if (ctx.hasUI) ctx.ui.notify(`Unknown LDGR loop ${tokens[0]}; known loops: ${choices.map((choice) => choice.slug).join(", ")}`, "warning");
            return;
          }
          selected = requested;
          tokens.shift();
        }
        if (!selected) {
          const envLoop = process.env.LDGR_ACTIVE_LOOP || process.env.LDGR_ACTIVE_ADAPTER || process.env.LDGR_ADAPTER_SLUG;
          if (envLoop) selected = choices.find((choice) => loopChoiceMatches(choice, envLoop));
        }
        if (!selected && choices.length === 1) selected = choices[0];
        if (!selected && ctx.hasUI) {
          const picked = await ctx.ui.select(
            "Select LDGR loop to run:",
            choices.map((choice) => `${choice.slug}${choice.title ? ` — ${choice.title}` : ""}`),
          );
          if (!picked) return;
          selected = choices.find((choice) => picked.startsWith(choice.slug));
        }
        if (!selected) {
          if (ctx.hasUI) ctx.ui.notify(`Multiple LDGR loops found; pass one of: ${choices.map((choice) => choice.slug).join(", ")}`, "warning");
          return;
        }

        const loopArgs = [
          "loop",
          "run",
          "--prompt",
          selected.promptPath,
          "--agent",
          "agentctl",
          "--stream-agent-output",
          "--until-empty",
          "--summary-agent",
          "agentctl",
          ...tokens,
        ];
        if (ctx.hasUI) ctx.ui.notify(`Running ${selected.slug} loop via ldgr ${loopArgs.join(" ")}`, "info");
        const result = await runLdgr(ctx.cwd, loopArgs, LOOP_TIMEOUT_MS);
        if (ctx.hasUI) ctx.ui.notify(`run-loop ${selected.slug} exited ${result.code ?? "signal"}`, result.code === 0 ? "info" : "warning");
        pi.sendUserMessage(renderRunLoopMessage(selected, loopArgs, result));
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        if (ctx.hasUI) ctx.ui.notify(`/run-loop failed: ${message}`, "warning");
      }
    },
  });

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
