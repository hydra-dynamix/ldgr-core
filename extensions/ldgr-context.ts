import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { execFile } from "node:child_process";
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

async function discoverAdapters(cwd: string): Promise<Adapter[]> {
  const result = await runLdgr(cwd, ["adapter", "list", "--json"]);
  if (result.code !== 0) {
    throw new Error(renderLdgrMessage(["adapter", "list", "--json"], result));
  }
  const registry = JSON.parse(result.stdout || "{}") as AdapterRegistry;
  return (registry.adapters ?? []).filter((adapter) => Boolean(adapter.profile?.loop_prompt_path));
}

function adapterMatches(adapter: Adapter, token: string): boolean {
  return adapter.slug === token || (adapter.aliases ?? []).includes(token);
}

function loopPromptPath(adapter: Adapter): string {
  const prompt = adapter.profile?.loop_prompt_path;
  if (!prompt) throw new Error(`adapter ${adapter.slug} does not declare profile.loop_prompt_path`);
  return join(adapter.root_path, prompt);
}

function renderRunLoopMessage(adapter: Adapter, commandArgs: string[], result: LdgrResult): string {
  return renderLdgrMessage(commandArgs, result)
    + `\n\nAdapter selected: ${adapter.slug}${adapter.title ? ` (${adapter.title})` : ""}`;
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
    description: "Detect the active LDGR adapter and run its loop through agentctl.",
    handler: async (argsText, ctx) => {
      try {
        const tokens = parseArgs(argsText.trim());
        const adapters = await discoverAdapters(ctx.cwd);
        if (adapters.length === 0) {
          if (ctx.hasUI) ctx.ui.notify("No installed LDGR adapters with loop prompts were discovered.", "warning");
          return;
        }

        let selected: Adapter | undefined;
        if (tokens.length > 0 && !tokens[0].startsWith("-")) {
          const requested = adapters.find((adapter) => adapterMatches(adapter, tokens[0]));
          if (!requested) {
            if (ctx.hasUI) ctx.ui.notify(`Unknown LDGR adapter ${tokens[0]}; known adapters: ${adapters.map((adapter) => adapter.slug).join(", ")}`, "warning");
            return;
          }
          selected = requested;
          tokens.shift();
        }
        if (!selected) {
          const envAdapter = process.env.LDGR_ACTIVE_ADAPTER || process.env.LDGR_ADAPTER_SLUG;
          if (envAdapter) selected = adapters.find((adapter) => adapterMatches(adapter, envAdapter));
        }
        if (!selected && adapters.length === 1) selected = adapters[0];
        if (!selected && ctx.hasUI) {
          const choice = await ctx.ui.select(
            "Select LDGR adapter loop to run:",
            adapters.map((adapter) => `${adapter.slug}${adapter.title ? ` — ${adapter.title}` : ""}`),
          );
          if (!choice) return;
          selected = adapters.find((adapter) => choice.startsWith(adapter.slug));
        }
        if (!selected) {
          if (ctx.hasUI) ctx.ui.notify(`Multiple LDGR adapters found; pass one of: ${adapters.map((adapter) => adapter.slug).join(", ")}`, "warning");
          return;
        }

        const loopArgs = [
          "loop",
          "run",
          "--prompt",
          loopPromptPath(selected),
          "--agent",
          "agentctl",
          "--stream-agent-output",
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
