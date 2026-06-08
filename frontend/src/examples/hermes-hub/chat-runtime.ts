import type {
  ExampleHubClient,
  ExampleHubConfig,
  ExampleSessionEvent,
} from "./hub-client";
import {
  clearStoredPendingToolResult,
  loadStoredPendingToolResult,
  saveStoredPendingToolResult,
  type StoredPendingToolResult,
} from "./storage";
import type { LocalToolRegistry } from "./tool-registry";

export type ExampleToolCallState = {
  requestId: string;
  toolName: string;
  status: "running" | "completed" | "failed" | "expired";
  summary: string;
  argumentsPreview: string;
  resultPreview?: string;
};

export type ChatRuntimeOptions = {
  client: ExampleHubClient;
  config: ExampleHubConfig;
  accessToken: string;
  sessionId: string;
  toolRegistry: LocalToolRegistry;
  onToolCallUpdate: (toolCall: ExampleToolCallState) => void;
  onSessionEvent: (event: ExampleSessionEvent) => void;
  onEventLog: (message: string) => void;
  onLocalToolSuccess?: (toolName: string) => void;
  onStreamDisconnect?: (error: Error) => void;
};

type ToolRequestPayload = {
  requestId: string;
  sessionId: string;
  integrationId: string;
  toolName: string;
  arguments: Record<string, unknown>;
  timeoutSeconds: number;
  expiresAt: number;
  status: "pending" | "completed" | "failed" | "expired";
  createdAt: number;
  updatedAt: number;
  resultMessageId?: string | null;
};

function formatArgumentsPreview(argumentsObject: Record<string, unknown>): string {
  try {
    return JSON.stringify(argumentsObject, null, 2);
  } catch {
    return "{}";
  }
}

function formatToolFailure(error: unknown): string {
  const message = error instanceof Error ? error.message : "未知错误";
  return `工具执行失败：${message}`;
}

function buildPendingToolResult(
  request: ToolRequestPayload,
  resultStatus: StoredPendingToolResult["resultStatus"],
  summary: string,
  resultText: string,
): StoredPendingToolResult {
  return {
    requestId: request.requestId,
    sessionId: request.sessionId,
    toolName: request.toolName,
    resultStatus,
    summary,
    resultText,
    argumentsPreview: formatArgumentsPreview(request.arguments),
    createdAt: Date.now(),
  };
}

function normalizeToolRequestPayload(input: unknown): ToolRequestPayload {
  const candidate =
    input && typeof input === "object" && "request" in input
      ? (input as { request: unknown }).request
      : input;
  if (!candidate || typeof candidate !== "object") {
    throw new Error("工具请求格式无效");
  }
  const request = candidate as Partial<ToolRequestPayload>;
  return {
    requestId: String((request as { request_id?: unknown }).request_id ?? request.requestId ?? ""),
    sessionId: String((request as { session_id?: unknown }).session_id ?? request.sessionId ?? ""),
    integrationId: String(
      (request as { integration_id?: unknown }).integration_id ?? request.integrationId ?? "",
    ),
    toolName: String((request as { tool_name?: unknown }).tool_name ?? request.toolName ?? ""),
    arguments:
      request.arguments && typeof request.arguments === "object" && !Array.isArray(request.arguments)
        ? request.arguments
        : {},
    timeoutSeconds: Number(
      (request as { timeout_seconds?: unknown }).timeout_seconds ?? request.timeoutSeconds ?? 0,
    ),
    expiresAt: Number(
      (request as { expires_at?: unknown }).expires_at ?? request.expiresAt ?? 0,
    ),
    status:
      request.status === "completed" ||
      request.status === "failed" ||
      request.status === "expired"
        ? request.status
        : "pending",
    createdAt: Number((request as { created_at?: unknown }).created_at ?? request.createdAt ?? 0),
    updatedAt: Number((request as { updated_at?: unknown }).updated_at ?? request.updatedAt ?? 0),
    resultMessageId:
      typeof (request as { result_message_id?: unknown }).result_message_id === "string"
        ? ((request as { result_message_id?: string }).result_message_id ?? null)
        : typeof request.resultMessageId === "string"
          ? request.resultMessageId
          : null,
  };
}

export function createChatRuntime(options: ChatRuntimeOptions) {
  const handledRequests = new Set<string>();
  const inflightRequests = new Set<string>();
  let stopListening: (() => void) | null = null;

  function emitToolState(toolCall: ExampleToolCallState) {
    options.onToolCallUpdate(toolCall);
  }

  async function submitPendingToolResult(
    request: ToolRequestPayload,
    pendingResult: StoredPendingToolResult,
    replayedFromStorage: boolean,
  ) {
    try {
      await options.client.submitBusinessToolResult(
        options.config,
        options.accessToken,
        options.sessionId,
        request.requestId,
        pendingResult.resultText,
      );
      handledRequests.add(request.requestId);
      clearStoredPendingToolResult(pendingResult.sessionId, pendingResult.requestId);
      emitToolState({
        requestId: request.requestId,
        toolName: request.toolName,
        status: pendingResult.resultStatus === "completed" ? "completed" : "failed",
        summary: pendingResult.summary,
        argumentsPreview: pendingResult.argumentsPreview,
        resultPreview: pendingResult.resultText,
      });
      options.onEventLog(
        replayedFromStorage
          ? `已补交暂存工具结果：${request.toolName}（${request.requestId}）`
          : `工具结果已回写：${request.toolName}（${request.requestId}）`,
      );
    } catch (error) {
      const message = error instanceof Error ? error.message : "未知错误";
      emitToolState({
        requestId: request.requestId,
        toolName: request.toolName,
        status: "failed",
        summary:
          pendingResult.resultStatus === "completed"
            ? "结果回写失败，本地工具已执行"
            : "工具执行失败，结果待重新回写",
        argumentsPreview: pendingResult.argumentsPreview,
        resultPreview: pendingResult.resultText,
      });
      options.onEventLog(
        `结果回写失败：${request.toolName}（${request.requestId}） - ${message}`,
      );
    }
  }

  async function handleToolRequest(rawRequest: unknown) {
    const request = normalizeToolRequestPayload(rawRequest);
    const pendingResult = loadStoredPendingToolResult(request.sessionId, request.requestId);

    if (request.status === "expired") {
      if (pendingResult) {
        clearStoredPendingToolResult(request.sessionId, request.requestId);
      }
      emitToolState({
        requestId: request.requestId,
        toolName: request.toolName,
        status: "expired",
        summary: "工具请求已过期",
        argumentsPreview: formatArgumentsPreview(request.arguments),
      });
      return;
    }
    if (request.status !== "pending") {
      if (pendingResult) {
        clearStoredPendingToolResult(request.sessionId, request.requestId);
      }
      handledRequests.add(request.requestId);
      return;
    }
    if (handledRequests.has(request.requestId) || inflightRequests.has(request.requestId)) {
      return;
    }

    inflightRequests.add(request.requestId);
    try {
      if (pendingResult) {
        emitToolState({
          requestId: request.requestId,
          toolName: request.toolName,
          status: "running",
          summary: "正在重新回写结果",
          argumentsPreview: pendingResult.argumentsPreview,
          resultPreview: pendingResult.resultText,
        });
        options.onEventLog(`检测到暂存工具结果，准备重新回写：${request.toolName}（${request.requestId}）`);
        await submitPendingToolResult(request, pendingResult, true);
        return;
      }

      emitToolState({
        requestId: request.requestId,
        toolName: request.toolName,
        status: "running",
        summary: "执行中",
        argumentsPreview: formatArgumentsPreview(request.arguments),
      });
      options.onEventLog(`收到工具调用：${request.toolName}（${request.requestId}）`);

      const result = await options.toolRegistry.runTool(request.toolName, request.arguments);
      const nextPendingResult = buildPendingToolResult(
        request,
        "completed",
        result.summary,
        result.resultText,
      );
      saveStoredPendingToolResult(nextPendingResult);
      options.onLocalToolSuccess?.(request.toolName);
      await submitPendingToolResult(request, nextPendingResult, false);
    } catch (error) {
      const failureText = formatToolFailure(error);
      const nextPendingResult = buildPendingToolResult(
        request,
        "failed",
        "工具执行失败",
        failureText,
      );
      saveStoredPendingToolResult(nextPendingResult);
      options.onEventLog(`工具执行失败：${request.toolName}（${request.requestId}）`);
      await submitPendingToolResult(request, nextPendingResult, false);
    } finally {
      inflightRequests.delete(request.requestId);
    }
  }

  function handleSessionEvent(event: ExampleSessionEvent) {
    options.onSessionEvent(event);
    if (event.type === "messages_snapshot") {
      for (const requestEvent of event.businessToolRequests) {
        void handleToolRequest(requestEvent);
      }
      return;
    }
    if (event.type === "business_tool_request") {
      void handleToolRequest(event);
    }
  }

  return {
    start() {
      stopListening = options.client.subscribeSessionEvents(
        options.config,
        options.accessToken,
        options.sessionId,
        handleSessionEvent,
        (error) => {
          options.onEventLog(`事件流断开：${error.message}`);
          options.onStreamDisconnect?.(error);
        },
      );
    },
    stop() {
      stopListening?.();
      stopListening = null;
    },
  };
}
