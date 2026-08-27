/**
 * `JourneyStep.data` is a different object per step type, and the server sends
 * it as a bare `data` key rather than a tagged union.
 *
 * These readers are that discrimination, in one place. The alternative — casting
 * at each use site, which is what the deleted web page did — spread
 * `data?.tool_name as string` over a dozen call sites, so a field renamed on the
 * wire failed nowhere and rendered `undefined` everywhere. Here a rename breaks
 * one file.
 *
 * Every reader is total: it takes `unknown` and answers a fully-populated
 * object, because a step whose payload did not decode should render as an empty
 * row rather than throw inside a map over hundreds of steps.
 */
import type { JourneyStep, TokenUsage } from "../../lib/types";

function obj(data: unknown): Record<string, unknown> {
  return typeof data === "object" && data !== null && !Array.isArray(data)
    ? (data as Record<string, unknown>)
    : {};
}

function str(data: Record<string, unknown>, key: string): string {
  const v = data[key];
  return typeof v === "string" ? v : "";
}

function num(data: Record<string, unknown>, key: string): number {
  const v = data[key];
  return typeof v === "number" ? v : 0;
}

function usage(data: Record<string, unknown>, key: string): TokenUsage | undefined {
  const v = data[key];
  return typeof v === "object" && v !== null ? (v as TokenUsage) : undefined;
}

export function textContent(step: JourneyStep): string {
  return str(obj(step.data), "content");
}

export interface ThinkingText {
  preview: string;
  full: string;
}

export function thinkingText(step: JourneyStep): ThinkingText {
  const d = obj(step.data);
  return { preview: str(d, "preview"), full: str(d, "full") };
}

export interface ToolCall {
  toolUseId: string;
  toolName: string;
  input: unknown;
  /** Set only when this call spawned a sub-agent nested under the step. */
  agentType: string;
  description: string;
  agentUsage: TokenUsage | undefined;
}

export function toolCall(step: JourneyStep): ToolCall {
  const d = obj(step.data);
  return {
    toolUseId: str(d, "tool_use_id"),
    toolName: str(d, "tool_name"),
    input: d.input,
    agentType: str(d, "agent_type"),
    description: str(d, "description"),
    agentUsage: usage(d, "agent_usage"),
  };
}

/** Whether a `tool_call` step delegated — the test the icon and label branch on. */
export function isDelegation(step: JourneyStep): boolean {
  const call = toolCall(step);
  return Boolean(call.agentType || call.description || step.steps?.length);
}

export interface ToolResult {
  toolUseId: string;
  content: string;
  isError: boolean;
}

export function toolResult(step: JourneyStep): ToolResult {
  const d = obj(step.data);
  return {
    toolUseId: str(d, "tool_use_id"),
    content: str(d, "content"),
    isError: d.is_error === true,
  };
}

export interface SubAgent {
  agentId: string;
  agentType: string;
  description: string;
  usage: TokenUsage | undefined;
}

export function subAgent(step: JourneyStep): SubAgent {
  const d = obj(step.data);
  return {
    agentId: str(d, "agent_id"),
    agentType: str(d, "agent_type"),
    description: str(d, "description"),
    usage: usage(d, "usage"),
  };
}

export interface Compaction {
  trigger: string;
  preTokens: number;
  postTokens: number;
  droppedTokens: number;
}

export function compaction(step: JourneyStep): Compaction {
  const d = obj(step.data);
  return {
    trigger: str(d, "trigger"),
    preTokens: num(d, "pre_tokens"),
    postTokens: num(d, "post_tokens"),
    droppedTokens: num(d, "dropped_tokens"),
  };
}

/** `in + out`, the pair every token figure in this view is stated as. */
export function conversationTokens(u: TokenUsage | undefined | null): number {
  return (u?.input_tokens ?? 0) + (u?.output_tokens ?? 0);
}
