// Sidecar: JSON-RPC 2.0 over stdin/stdout.
// Stdout is exclusively the protocol channel; all logs go to stderr.
import { query, type SDKResultMessage } from "@anthropic-ai/claude-agent-sdk";

// Belt-and-suspenders: strip API key so the SDK uses the bundled Claude Code
// OAuth session (Max subscription) even when run standalone by a developer.
delete process.env.ANTHROPIC_API_KEY;

// Per-method Claude model, set by the Rust supervisor from `[llm].*_model`.
const DEFAULT_MODEL = "claude-sonnet-4-5";
const RELATION_MODEL = process.env.SMRITI_RELATION_MODEL || DEFAULT_MODEL;
const ANCHOR_MODEL = process.env.SMRITI_ANCHOR_MODEL || DEFAULT_MODEL;
const CONSOLIDATION_MODEL = process.env.SMRITI_CONSOLIDATION_MODEL || DEFAULT_MODEL;

import { z } from "zod";
import * as readline from "node:readline";

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 types
// ---------------------------------------------------------------------------

interface JsonRpcRequest {
  jsonrpc: "2.0";
  id: string | number | null;
  method: string;
  params: unknown;
}

interface JsonRpcSuccess {
  jsonrpc: "2.0";
  id: string | number | null;
  result: unknown;
}

interface JsonRpcError {
  jsonrpc: "2.0";
  id: string | number | null;
  error: { code: number; message: string; data?: unknown };
}

type JsonRpcResponse = JsonRpcSuccess | JsonRpcError;

// ---------------------------------------------------------------------------
// Error codes
// ---------------------------------------------------------------------------

const ERR_PARSE = -32700;
const ERR_INVALID_REQUEST = -32600;
const ERR_METHOD_NOT_FOUND = -32601;
const ERR_INVALID_PARAMS = -32602;
const ERR_INTERNAL = -32603;

// ---------------------------------------------------------------------------
// Zod schemas for method params
// ---------------------------------------------------------------------------

const ExtractRelationsParams = z.object({
  memory_id: z.string(),
  summary: z.string(),
  content: z.string().optional(),
  project: z.string().optional(),
  domain: z.string().optional(),
});

// content intentionally omitted — injection defense
const ExtractAnchorsParams = z.object({
  memory_id: z.string(),
  summary: z.string(),
  project: z.string().optional(),
  domain: z.string().optional(),
});

const SynthesizeConsolidationParams = z.object({
  cluster_id: z.string(),
  summaries: z.array(z.string()).min(2).max(12),
  subtype: z.string(),
  project: z.string().nullable().optional(),
  domain: z.string().nullable().optional(),
});

const HealthParams = z.object({}).passthrough();

// ---------------------------------------------------------------------------
// Safe error message — strips prompt material from error objects before they
// are embedded in a JSON-RPC response (and forwarded to Rust logging).
//
// Only the error's `name` and numeric `code` are included in the returned
// string; the free-form `message` is deliberately omitted because SDK errors
// can echo prompt bodies (PromptTooLongError, rate-limit bodies, etc.).
// The full error object is written to stderr so the operator journal captures
// it without Rust ever forwarding it.
// ---------------------------------------------------------------------------

function safeErrorMessage(e: unknown): string {
  // Always log the full error to operator stderr — never to stdout/JSON-RPC.
  process.stderr.write(`[sidecar] error detail: ${JSON.stringify(e, null, 0)}\n`);

  if (!(e instanceof Error)) {
    return "unknown_error";
  }

  // Use only the name (e.g. "PromptTooLongError", "AbortError") — never message.
  const name = e.name ?? "Error";
  const code = (e as Error & { code?: unknown }).code;

  let result = name;
  if (typeof code === "string" || typeof code === "number") {
    result += `:${String(code)}`;
  }

  // Strip non-printable and non-ASCII characters, then truncate to 80 chars.
  result = result.replace(/[^\x20-\x7E]/g, "").slice(0, 80);

  return result || "unknown_error";
}

// ---------------------------------------------------------------------------
// Relation extraction — schema and prompt constants
// ---------------------------------------------------------------------------

const RELATION_SCHEMA: Record<string, unknown> = {
  type: "object",
  properties: {
    relations: {
      type: "array",
      items: {
        type: "object",
        required: ["from", "to", "kind", "confidence"],
        properties: {
          from: { type: "string" },
          to: { type: "string" },
          kind: {
            type: "string",
            enum: [
              "causes",
              "enables",
              "blocks",
              "follows",
              "precedes",
              "similar_to",
              "contradicts",
              "mentions",
              "depends_on",
              "part_of",
            ],
          },
          confidence: { type: "number", minimum: 0, maximum: 1 },
        },
      },
    },
  },
  required: ["relations"],
};

const SYSTEM_PROMPT =
  "You extract structured semantic relations between memories. " +
  "Output only valid JSON matching the schema. The 'from' field MUST be the input memory_id verbatim. " +
  "The 'to' field is another memory_id you reference; if you can't reference a known memory, omit that relation. " +
  "Use confidence in [0,1] reflecting your certainty. Return at most 5 relations. " +
  "If no high-confidence relations exist, return an empty array.";

// ---------------------------------------------------------------------------
// Anchor entity extraction — schema and prompt constants
// ---------------------------------------------------------------------------

const ANCHOR_SCHEMA: Record<string, unknown> = {
  type: "object",
  properties: {
    anchors: {
      type: "array",
      items: { type: "string" },
      minItems: 0,
      maxItems: 3,
    },
  },
  required: ["anchors"],
  additionalProperties: false,
};

const ANCHOR_SYSTEM_PROMPT =
  "Extract up to 3 anchor entities from the memory summary. Anchors are:\n" +
  "- proper nouns (people, places, organizations)\n" +
  "- project names, codenames, product names\n" +
  "- technology names, system names\n" +
  "- event names, meeting names\n" +
  "Return raw text as it appears (no lowercasing, no stemming). " +
  "If fewer than 3 strong anchors exist, return what is confident. " +
  "Return [] if no entities are clear. Output JSON only.";

// ---------------------------------------------------------------------------
// synthesize_consolidation — schema and prompt constants
// ---------------------------------------------------------------------------

const CONSOLIDATION_SCHEMA: Record<string, unknown> = {
  type: "object",
  properties: {
    should_merge: { type: "boolean" },
    confidence: { type: "number", minimum: 0, maximum: 1 },
    merged_summary: { type: ["string", "null"] },
    reason: { type: "string" },
  },
  required: ["should_merge", "confidence", "merged_summary", "reason"],
  additionalProperties: false,
};

const CONSOLIDATION_SYSTEM_PROMPT =
  "You are a memory consolidator. You are given several memory summaries from the same project, same subtype, same author. " +
  "Decide whether they describe the same underlying fact/decision/event such that merging them into one record preserves information without distortion.\n\n" +
  "Rules:\n" +
  "- Only merge if the summaries are clearly redundant or describe the same evolving fact. " +
  "Different facts that happen to share keywords MUST NOT be merged.\n" +
  "- If you merge, write a concise (10-500 chars) summary that captures every distinct piece of information across the inputs. " +
  "Do not invent details. Preserve specifics (names, numbers, dates).\n" +
  "- If unsure, set should_merge=false and explain in reason. Confidence below 0.75 means caller will skip; bias toward caution.\n" +
  "- The merged summary will be the only thing surfaced on recall. Originals will be archived but reachable via a derives graph link.";

// ---------------------------------------------------------------------------
// extract_relations implementation
// ---------------------------------------------------------------------------

async function extractRelations(
  params: z.infer<typeof ExtractRelationsParams>,
): Promise<DispatchResult> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 25_000);

  try {
    const promptLines = [
      `memory_id: ${params.memory_id}`,
      `summary: ${params.summary}`,
      params.content ? `content: ${params.content}` : "",
      params.project ? `project: ${params.project}` : "",
      params.domain ? `domain: ${params.domain}` : "",
    ].filter((l) => l.length > 0);
    const prompt = promptLines.join("\n");

    const gen = query({
      prompt,
      options: {
        model: RELATION_MODEL,
        systemPrompt: SYSTEM_PROMPT,
        allowedTools: [],
        permissionMode: "dontAsk",
        outputFormat: { type: "json_schema", schema: RELATION_SCHEMA },
        abortController: controller,
        maxBudgetUsd: 0.05,
      },
    });

    let lastResult: SDKResultMessage | undefined;
    for await (const msg of gen) {
      if (msg.type === "result") {
        lastResult = msg;
      }
    }

    if (!lastResult) {
      return { error: { code: ERR_INTERNAL, message: "agent_error:no_result" } };
    }

    if (lastResult.subtype === "success") {
      process.stderr.write(
        `[sidecar] extract_relations id=${params.memory_id} cost_usd=${lastResult.total_cost_usd} tokens=${lastResult.usage.output_tokens}\n`,
      );
      return { result: lastResult.structured_output };
    }

    return {
      error: {
        code: ERR_INTERNAL,
        message: `agent_error:${lastResult.subtype}`,
      },
    };
  } catch (e) {
    return {
      error: {
        code: ERR_INTERNAL,
        message: `extract_failed:${safeErrorMessage(e)}`,
      },
    };
  } finally {
    clearTimeout(timer);
  }
}

// ---------------------------------------------------------------------------
// extract_anchors implementation
// ---------------------------------------------------------------------------

async function extractAnchors(
  params: z.infer<typeof ExtractAnchorsParams>,
): Promise<DispatchResult> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 15_000);

  try {
    const promptLines = [
      `memory_id: ${params.memory_id}`,
      `summary: ${params.summary}`,
      params.project ? `project: ${params.project}` : "",
      params.domain ? `domain: ${params.domain}` : "",
    ].filter((l) => l.length > 0);
    const prompt = promptLines.join("\n");

    const gen = query({
      prompt,
      options: {
        model: ANCHOR_MODEL,
        systemPrompt: ANCHOR_SYSTEM_PROMPT,
        allowedTools: [],
        permissionMode: "dontAsk",
        outputFormat: { type: "json_schema", schema: ANCHOR_SCHEMA },
        abortController: controller,
        maxBudgetUsd: 0.02,
      },
    });

    let lastResult: SDKResultMessage | undefined;
    for await (const msg of gen) {
      if (msg.type === "result") {
        lastResult = msg;
      }
    }

    if (!lastResult) {
      return { error: { code: ERR_INTERNAL, message: "agent_error:no_result" } };
    }

    if (lastResult.subtype === "success") {
      process.stderr.write(
        `[sidecar] extract_anchors id=${params.memory_id} cost_usd=${lastResult.total_cost_usd} tokens=${lastResult.usage.output_tokens}\n`,
      );
      return { result: lastResult.structured_output };
    }

    return {
      error: {
        code: ERR_INTERNAL,
        message: `agent_error:${lastResult.subtype}`,
      },
    };
  } catch (e) {
    return {
      error: {
        code: ERR_INTERNAL,
        message: `extract_failed:${safeErrorMessage(e)}`,
      },
    };
  } finally {
    clearTimeout(timer);
  }
}

// ---------------------------------------------------------------------------
// synthesize_consolidation implementation
// ---------------------------------------------------------------------------

async function synthesizeConsolidation(
  params: z.infer<typeof SynthesizeConsolidationParams>,
): Promise<DispatchResult> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 25_000);

  try {
    const numberedSummaries = params.summaries
      .map((s, i) => `${i + 1}. ${s}`)
      .join("\n");

    const promptParts = [
      `cluster_id: ${params.cluster_id}`,
      `subtype: ${params.subtype}`,
      params.project ? `project: ${params.project}` : "",
      params.domain ? `domain: ${params.domain}` : "",
      "",
      "summaries:",
      numberedSummaries,
    ].filter((l, i) => i < 4 || l.length > 0);
    const prompt = promptParts.join("\n");

    const gen = query({
      prompt,
      options: {
        model: CONSOLIDATION_MODEL,
        systemPrompt: CONSOLIDATION_SYSTEM_PROMPT,
        allowedTools: [],
        permissionMode: "dontAsk",
        outputFormat: { type: "json_schema", schema: CONSOLIDATION_SCHEMA },
        abortController: controller,
        maxBudgetUsd: 0.05,
      },
    });

    let lastResult: SDKResultMessage | undefined;
    for await (const msg of gen) {
      if (msg.type === "result") {
        lastResult = msg;
      }
    }

    if (!lastResult) {
      return { error: { code: ERR_INTERNAL, message: "agent_error:no_result" } };
    }

    if (lastResult.subtype === "success") {
      process.stderr.write(
        `[sidecar] synthesize_consolidation cluster_id=${params.cluster_id} summaries=${params.summaries.length} cost_usd=${lastResult.total_cost_usd} tokens=${lastResult.usage.output_tokens}\n`,
      );
      return { result: lastResult.structured_output };
    }

    return {
      error: {
        code: ERR_INTERNAL,
        message: `agent_error:${lastResult.subtype}`,
      },
    };
  } catch (e) {
    return {
      error: {
        code: ERR_INTERNAL,
        message: `synthesize_failed:${safeErrorMessage(e)}`,
      },
    };
  } finally {
    clearTimeout(timer);
  }
}

// ---------------------------------------------------------------------------
// Method dispatch
// ---------------------------------------------------------------------------

type DispatchResult = { result: unknown } | { error: { code: number; message: string; data?: unknown } };

async function dispatch(method: string, params: unknown): Promise<DispatchResult> {
  switch (method) {
    case "health": {
      const parsed = HealthParams.safeParse(params);
      if (!parsed.success) {
        return { error: { code: ERR_INVALID_PARAMS, message: "Invalid params", data: parsed.error.issues } };
      }
      return { result: { ok: true, version: "0.1.0" } };
    }

    case "extract_relations": {
      const parsed = ExtractRelationsParams.safeParse(params);
      if (!parsed.success) {
        return { error: { code: ERR_INVALID_PARAMS, message: "Invalid params", data: parsed.error.issues } };
      }
      return extractRelations(parsed.data);
    }

    case "extract_anchors": {
      const parsed = ExtractAnchorsParams.safeParse(params);
      if (!parsed.success) {
        return { error: { code: ERR_INVALID_PARAMS, message: "Invalid params", data: parsed.error.issues } };
      }
      return extractAnchors(parsed.data);
    }

    case "synthesize_consolidation": {
      const parsed = SynthesizeConsolidationParams.safeParse(params);
      if (!parsed.success) {
        return { error: { code: ERR_INVALID_PARAMS, message: "Invalid params", data: parsed.error.issues } };
      }
      return synthesizeConsolidation(parsed.data);
    }

    default:
      return { error: { code: ERR_METHOD_NOT_FOUND, message: `Method not found: ${method}` } };
  }
}

// ---------------------------------------------------------------------------
// I/O helpers — all protocol output goes through writeResponse so stdout is
// never used for logs.
// ---------------------------------------------------------------------------

function writeResponse(resp: JsonRpcResponse): void {
  process.stdout.write(JSON.stringify(resp) + "\n");
}

function makeError(
  id: string | number | null,
  code: number,
  message: string,
  data?: unknown,
): JsonRpcError {
  const error: JsonRpcError["error"] = { code, message };
  if (data !== undefined) error.data = data;
  return { jsonrpc: "2.0", id, error };
}

// ---------------------------------------------------------------------------
// Request handler
// ---------------------------------------------------------------------------

async function handleLine(raw: string): Promise<void> {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    writeResponse(makeError(null, ERR_PARSE, "Parse error"));
    return;
  }

  // Validate envelope shape.
  if (
    typeof parsed !== "object" ||
    parsed === null ||
    (parsed as Record<string, unknown>)["jsonrpc"] !== "2.0" ||
    typeof (parsed as Record<string, unknown>)["method"] !== "string"
  ) {
    writeResponse(makeError(null, ERR_INVALID_REQUEST, "Invalid Request"));
    return;
  }

  const req = parsed as JsonRpcRequest;
  const id = req.id ?? null;

  // Notifications (no id): handle but do not respond.
  const isNotification = !("id" in (parsed as object));
  if (isNotification) {
    process.stderr.write(`[sidecar] notification received: ${req.method}\n`);
    return;
  }

  process.stderr.write(`[sidecar] request id=${String(id)} method=${req.method}\n`);

  const outcome = await dispatch(req.method, req.params ?? {});

  if ("result" in outcome) {
    writeResponse({ jsonrpc: "2.0", id, result: outcome.result });
  } else {
    writeResponse({ jsonrpc: "2.0", id, error: outcome.error });
  }
}

// ---------------------------------------------------------------------------
// Main loop — line-delimited JSON over stdin
// ---------------------------------------------------------------------------

const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });

rl.on("line", (line) => {
  const trimmed = line.trim();
  if (trimmed.length === 0) return;
  // Fire-and-forget each request; errors surface through writeResponse.
  void handleLine(trimmed);
});

rl.on("close", () => {
  // stdin EOF — drain any buffered writes, then exit cleanly.
  process.stderr.write("[sidecar] stdin closed, exiting\n");
  process.exit(0);
});

// Flush pending writes on SIGTERM and exit cleanly.
process.on("SIGTERM", () => {
  process.stderr.write("[sidecar] SIGTERM received, shutting down\n");
  process.stdout.write("", () => process.exit(0));
});
