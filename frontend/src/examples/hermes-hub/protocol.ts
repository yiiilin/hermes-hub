import type { ExampleBusinessToolRequestEvent, ExampleMessage } from "./hub-client";

export const BUSINESS_TOOL_REQUEST_MARKER = "<!-- hermes-hub:business-tool-request:v1 -->";

export function isBusinessToolRequestMessageContent(content: string): boolean {
  return content.trimStart().startsWith(BUSINESS_TOOL_REQUEST_MARKER);
}

export function isBusinessToolRequestProtocolMessage(message: ExampleMessage): boolean {
  return isBusinessToolRequestMessageContent(message.content);
}

export function summarizeToolArguments(argumentsValue: Record<string, unknown>): string {
  const parts = Object.entries(argumentsValue)
    .filter(([, value]) => value !== undefined && value !== null && value !== "")
    .map(([key, value]) => `${key}=${formatArgumentValue(value)}`);

  return parts.length > 0 ? parts.join(", ") : "无参数";
}

export function businessToolRequestCreatedAt(event: ExampleBusinessToolRequestEvent): number {
  return event.request.createdAt;
}

function formatArgumentValue(value: unknown): string {
  if (typeof value === "string") {
    return value.length > 48 ? `${value.slice(0, 45)}...` : value;
  }
  if (Array.isArray(value)) {
    return value.join("|");
  }
  return JSON.stringify(value);
}
