use axum::{
    extract::State,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};

use crate::{session::store::ApiManagementSettings, AppState};

use super::ApiError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/docs", get(swagger_ui))
        .route("/api/docs/openapi.json", get(openapi_json))
}

async fn swagger_ui(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    ensure_api_management_enabled(&state).await?;
    Ok(Html(swagger_ui_html()))
}

async fn openapi_json(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    let settings = ensure_api_management_enabled(&state).await?;
    Ok(Json(openapi_spec(&state.config.cookie_name, &settings)))
}

async fn ensure_api_management_enabled(
    state: &AppState,
) -> Result<ApiManagementSettings, ApiError> {
    let settings = state
        .store
        .system_settings()
        .await
        .map_err(|_| ApiError::Internal)?;
    if !settings.api_management.enabled {
        return Err(ApiError::NotFound("api docs are not enabled"));
    }
    Ok(settings.api_management)
}

fn swagger_ui_html() -> String {
    // Swagger UI 直接从 CDN 加载，后端只负责同源提供 OpenAPI JSON；
    // 这样不用再把大块前端依赖塞进站点构建链。
    r##"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Hermes Hub API</title>
    <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5/swagger-ui.css" />
    <style>
      html,
      body,
      #swagger-ui {
        height: 100%;
        margin: 0;
      }
      body {
        background: #f8fafc;
      }
    </style>
  </head>
  <body>
    <div id="swagger-ui"></div>
    <script src="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5/swagger-ui-bundle.js"></script>
    <script>
      window.onload = () => {
        window.ui = SwaggerUIBundle({
          url: "/api/docs/openapi.json",
          dom_id: "#swagger-ui",
          deepLinking: true,
          displayRequestDuration: true,
          docExpansion: "list",
          presets: [SwaggerUIBundle.presets.apis],
          layout: "BaseLayout"
        });
      };
    </script>
  </body>
</html>"##
        .to_string()
}

fn openapi_spec(cookie_name: &str, settings: &ApiManagementSettings) -> Value {
    let _ = settings;
    json!({
        "openapi": "3.0.3",
        "info": {
            "title": "Hermes Hub Integration API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "OpenAPI for external business systems. Channel identifiers are managed internally by Hermes Hub and are intentionally not part of this public contract."
        },
        "servers": [
            { "url": "/" }
        ],
        "tags": [
            { "name": "OAuth", "description": "Business OAuth authorization flow" },
            { "name": "Integration Apps", "description": "Machine-authenticated integration app management" },
            { "name": "Integrations", "description": "Session APIs for external systems" }
        ],
        "components": {
            "securitySchemes": {
                "basicAuth": {
                    "type": "http",
                    "scheme": "basic"
                },
                "bearerAuth": {
                    "type": "http",
                    "scheme": "bearer"
                },
                "webSession": {
                    "type": "apiKey",
                    "in": "cookie",
                    "name": cookie_name
                }
            },
            "schemas": {
                "ErrorResponse": {
                    "type": "object",
                    "required": ["error", "message"],
                    "properties": {
                        "error": { "type": "string" },
                        "message": { "type": "string" },
                        "max_sessions_per_user": { "type": "integer", "format": "int32", "nullable": true }
                    }
                },
                "TokenResponse": {
                    "type": "object",
                    "required": ["access_token", "token_type", "expires_in", "scope"],
                    "properties": {
                        "access_token": { "type": "string" },
                        "token_type": { "type": "string", "example": "Bearer" },
                        "expires_in": { "type": "integer", "format": "int64" },
                        "scope": { "type": "string" }
                    }
                },
                "UserInfoResponse": {
                    "type": "object",
                    "required": ["id", "sub", "email", "integration_id", "toolset_names"],
                    "properties": {
                        "id": { "type": "string" },
                        "sub": { "type": "string" },
                        "email": { "type": "string", "format": "email" },
                        "integration_id": { "type": "string" },
                        "toolset_names": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    }
                },
                "IntegrationToolDefinition": {
                    "type": "object",
                    "required": ["name", "description", "parameters", "created_at", "updated_at"],
                    "properties": {
                        "name": { "type": "string" },
                        "description": { "type": "string" },
                        "parameters": {
                            "type": "object",
                            "additionalProperties": true
                        },
                        "created_at": { "type": "integer", "format": "int64" },
                        "updated_at": { "type": "integer", "format": "int64" }
                    }
                },
                "IntegrationToolsResponse": {
                    "type": "object",
                    "required": ["tools"],
                    "properties": {
                        "tools": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/IntegrationToolDefinition" }
                        }
                    }
                },
                "ReplaceIntegrationToolsRequest": {
                    "type": "object",
                    "required": ["tools"],
                    "properties": {
                        "tools": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "required": ["name", "description", "parameters"],
                                "properties": {
                                    "name": { "type": "string" },
                                    "description": { "type": "string" },
                                    "parameters": {
                                        "type": "object",
                                        "additionalProperties": true
                                    }
                                }
                            }
                        }
                    }
                },
                "IntegrationSession": {
                    "type": "object",
                    "required": ["id", "hidden_from_web", "created_at", "updated_at"],
                    "properties": {
                        "id": { "type": "string" },
                        "title": { "type": "string", "nullable": true },
                        "is_home": { "type": "boolean" },
                        "deletable": { "type": "boolean" },
                        "hidden_from_web": { "type": "boolean" },
                        "created_at": { "type": "integer", "format": "int64" },
                        "updated_at": { "type": "integer", "format": "int64" }
                    }
                },
                "IntegrationSessionListResponse": {
                    "type": "object",
                    "required": ["sessions"],
                    "properties": {
                        "sessions": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/IntegrationSession" }
                        }
                    }
                },
                "IntegrationSessionResponse": {
                    "type": "object",
                    "required": ["session"],
                    "properties": {
                        "session": { "$ref": "#/components/schemas/IntegrationSession" }
                    }
                },
                "CreateSessionRequest": {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["chat", "agent"], "default": "agent" },
                        "title": { "type": "string", "nullable": true }
                    }
                },
                "Attachment": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "name": { "type": "string" },
                        "content_type": { "type": "string" },
                        "kind": { "type": "string", "enum": ["file", "image"] },
                        "size": { "type": "integer", "format": "int64" },
                        "download_url": { "type": "string" }
                    }
                },
                "ChannelMessage": {
                    "type": "object",
                    "required": ["id", "session_id", "role", "content", "attachments", "created_at"],
                    "properties": {
                        "id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "role": { "type": "string", "enum": ["user", "assistant"] },
                        "message_kind": { "type": "string", "enum": ["text", "execution"], "nullable": true },
                        "client_message_key": { "type": "string", "nullable": true },
                        "content": { "type": "string" },
                        "attachments": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/Attachment" }
                        },
                        "created_at": { "type": "integer", "format": "int64" },
                        "updated_at": { "type": "integer", "format": "int64", "nullable": true }
                    }
                },
                "ChannelActiveRun": {
                    "type": "object",
                    "required": ["run_id", "status", "created_at", "updated_at"],
                    "properties": {
                        "run_id": { "type": "string" },
                        "status": { "type": "string" },
                        "output": { "type": "string", "nullable": true },
                        "error": { "type": "string", "nullable": true },
                        "output_message_id": { "type": "string", "nullable": true },
                        "created_at": { "type": "integer", "format": "int64" },
                        "updated_at": { "type": "integer", "format": "int64" }
                    }
                },
                "MessageResponse": {
                    "type": "object",
                    "required": ["message"],
                    "properties": {
                        "message": { "$ref": "#/components/schemas/ChannelMessage" }
                    }
                },
                "AttachmentListResponse": {
                    "type": "object",
                    "required": ["attachments"],
                    "properties": {
                        "attachments": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/Attachment" }
                        }
                    }
                },
                "AppendMessageRequest": {
                    "type": "object",
                    "required": ["role", "content"],
                    "properties": {
                        "role": { "type": "string", "enum": ["user", "assistant"] },
                        "content": { "type": "string" },
                        "attachments": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/Attachment" }
                        },
                        "client_message_key": { "type": "string", "nullable": true }
                    }
                },
                "UpdateMessageRequest": {
                    "type": "object",
                    "required": ["content"],
                    "properties": {
                        "content": { "type": "string" },
                        "attachments": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/Attachment" }
                        }
                    }
                },
                "BusinessToolRequestStatus": {
                    "type": "string",
                    "enum": ["pending", "completed", "failed", "expired"]
                },
                "BusinessToolRequestEvent": {
                    "type": "object",
                    "required": [
                        "type",
                        "request"
                    ],
                    "properties": {
                        "type": { "type": "string", "example": "business_tool_request" },
                        "request": {
                            "type": "object",
                            "required": [
                                "request_id",
                                "session_id",
                                "integration_id",
                                "tool_name",
                                "arguments",
                                "timeout_seconds",
                                "expires_at",
                                "status",
                                "created_at",
                                "updated_at"
                            ],
                            "properties": {
                                "request_id": { "type": "string" },
                                "session_id": { "type": "string" },
                                "integration_id": { "type": "string" },
                                "tool_name": { "type": "string" },
                                "arguments": {
                                    "type": "object",
                                    "additionalProperties": true
                                },
                                "timeout_seconds": {
                                    "type": "integer",
                                    "format": "int64",
                                    "description": "Effective timeout after applying the integration app default/max settings."
                                },
                                "expires_at": {
                                    "type": "integer",
                                    "format": "int64",
                                    "description": "Unix timestamp in seconds."
                                },
                                "status": { "$ref": "#/components/schemas/BusinessToolRequestStatus" },
                                "created_at": { "type": "integer", "format": "int64" },
                                "updated_at": { "type": "integer", "format": "int64" },
                                "result_message_id": { "type": "string", "nullable": true }
                            }
                        }
                    }
                },
                "BusinessToolResultRequest": {
                    "type": "object",
                    "required": ["result"],
                    "properties": {
                        "result": { "type": "string" }
                    }
                },
                "BusinessToolRequestResultRequest": {
                    "$ref": "#/components/schemas/BusinessToolResultRequest"
                },
                "MessagesSnapshotResponse": {
                    "type": "object",
                    "required": ["type", "messages", "active_run", "session", "business_tool_requests"],
                    "properties": {
                        "type": { "type": "string", "example": "messages_snapshot" },
                        "messages": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/ChannelMessage" }
                        },
                        "active_run": {
                            "allOf": [
                                { "$ref": "#/components/schemas/ChannelActiveRun" }
                            ],
                            "nullable": true
                        },
                        "session": { "$ref": "#/components/schemas/IntegrationSession" },
                        "business_tool_requests": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/BusinessToolRequestEvent" }
                        }
                    }
                }
            }
        },
        "paths": {
            "/api/integrations/apps/self/tools": {
                "get": {
                    "tags": ["Integration Apps"],
                    "summary": "List tool definitions synced for the current integration app",
                    "security": [{ "basicAuth": [] }],
                    "responses": {
                        "200": { "description": "Current tool definitions", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/IntegrationToolsResponse" } } } },
                        "401": { "description": "Integration app client credentials required", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                        "404": { "description": "Integration app is disabled", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } }
                    }
                },
                "put": {
                    "tags": ["Integration Apps"],
                    "summary": "Replace tool definitions for the current integration app",
                    "description": "Business systems should publish their full current tool list to Hermes Hub instead of relying on manual admin-side editing.",
                    "security": [{ "basicAuth": [] }],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/ReplaceIntegrationToolsRequest" }
                            }
                        }
                    },
                    "responses": {
                        "200": { "description": "Updated tool definitions", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/IntegrationToolsResponse" } } } },
                        "400": { "description": "Invalid tool definitions", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                        "401": { "description": "Integration app client credentials required", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                        "404": { "description": "Integration app is disabled", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } }
                    }
                }
            },
            "/api/oauth/authorize": {
                "get": {
                    "tags": ["OAuth"],
                    "summary": "Start business OAuth authorization",
                    "security": [{ "webSession": [] }],
                    "parameters": [
                        { "name": "response_type", "in": "query", "required": true, "schema": { "type": "string", "example": "code" } },
                        { "name": "client_id", "in": "query", "required": true, "schema": { "type": "string" } },
                        {
                            "name": "redirect_uri",
                            "in": "query",
                            "required": true,
                            "description": "Must exactly match the integration app callback URL.",
                            "schema": { "type": "string", "format": "uri" }
                        },
                        { "name": "scope", "in": "query", "required": false, "schema": { "type": "string" } },
                        { "name": "state", "in": "query", "required": false, "schema": { "type": "string" } }
                    ],
                    "responses": {
                        "302": { "description": "Redirect with authorization code" },
                        "400": { "description": "Invalid request", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                        "401": { "description": "Unauthorized client or user", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } }
                    }
                }
            },
            "/api/oauth/token": {
                "post": {
                    "tags": ["OAuth"],
                    "summary": "Exchange authorization code for bearer token",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/x-www-form-urlencoded": {
                                "schema": {
                                    "type": "object",
                                    "required": ["grant_type", "client_id", "client_secret", "redirect_uri", "code"],
                                    "properties": {
                                        "grant_type": { "type": "string", "example": "authorization_code" },
                                        "client_id": { "type": "string" },
                                        "client_secret": { "type": "string" },
                                        "redirect_uri": {
                                            "type": "string",
                                            "format": "uri",
                                            "description": "Must exactly match the integration app callback URL."
                                        },
                                        "code": { "type": "string" }
                                    }
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": { "description": "Bearer token", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/TokenResponse" } } } },
                        "400": { "description": "Invalid request", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                        "401": { "description": "Invalid credentials or code", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } }
                    }
                }
            },
            "/api/oauth/userinfo": {
                "get": {
                    "tags": ["OAuth"],
                    "summary": "Inspect current OAuth subject",
                    "security": [{ "bearerAuth": [] }],
                    "responses": {
                        "200": { "description": "Current OAuth subject", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/UserInfoResponse" } } } },
                        "401": { "description": "Bearer token required", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } }
                    }
                }
            },
            "/api/integrations/sessions": {
                "get": {
                    "tags": ["Integrations"],
                    "summary": "List integration sessions for the OAuth user and client",
                    "security": [{ "bearerAuth": [] }],
                    "responses": {
                        "200": { "description": "Session list", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/IntegrationSessionListResponse" } } } },
                        "401": { "description": "OAuth bearer token required", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } }
                    }
                },
                "post": {
                    "tags": ["Integrations"],
                    "summary": "Create an integration session",
                    "security": [{ "bearerAuth": [] }],
                    "requestBody": {
                        "required": false,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/CreateSessionRequest" }
                            }
                        }
                    },
                    "responses": {
                        "201": { "description": "Created session", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/IntegrationSessionResponse" } } } },
                        "401": { "description": "OAuth bearer token required", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } }
                    }
                }
            },
            "/api/integrations/sessions/{session_id}": {
                "delete": {
                    "tags": ["Integrations"],
                    "summary": "Delete an integration session",
                    "security": [{ "bearerAuth": [] }],
                    "parameters": [
                        { "name": "session_id", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "responses": {
                        "204": { "description": "Deleted" },
                        "401": { "description": "OAuth bearer token required", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                        "404": { "description": "Session not found", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } }
                    }
                }
            },
            "/api/integrations/sessions/{session_id}/messages": {
                "post": {
                    "tags": ["Integrations"],
                    "summary": "Append a message and enqueue a Hermes run for user messages",
                    "security": [{ "bearerAuth": [] }],
                    "parameters": [
                        { "name": "session_id", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/AppendMessageRequest" }
                            }
                        }
                    },
                    "responses": {
                        "201": { "description": "Created message", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/MessageResponse" } } } },
                        "401": { "description": "OAuth bearer token required", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                        "404": { "description": "Session not found", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } }
                    }
                }
            },
            "/api/integrations/sessions/{session_id}/messages/{message_id}": {
                "put": {
                    "tags": ["Integrations"],
                    "summary": "Update an integration session message",
                    "security": [{ "bearerAuth": [] }],
                    "parameters": [
                        { "name": "session_id", "in": "path", "required": true, "schema": { "type": "string" } },
                        { "name": "message_id", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/UpdateMessageRequest" }
                            }
                        }
                    },
                    "responses": {
                        "200": { "description": "Updated message", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/MessageResponse" } } } },
                        "401": { "description": "OAuth bearer token required", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                        "404": { "description": "Message not found", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } }
                    }
                }
            },
            "/api/integrations/sessions/{session_id}/attachments": {
                "post": {
                    "tags": ["Integrations"],
                    "summary": "Upload input attachments for an integration session",
                    "security": [{ "bearerAuth": [] }],
                    "parameters": [
                        { "name": "session_id", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "responses": {
                        "201": { "description": "Uploaded attachments", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/AttachmentListResponse" } } } },
                        "401": { "description": "OAuth bearer token required", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                        "404": { "description": "Session not found", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } }
                    }
                }
            },
            "/api/integrations/sessions/{session_id}/events": {
                "get": {
                    "tags": ["Integrations"],
                    "summary": "Subscribe to session event stream, including business tool requests",
                    "security": [{ "bearerAuth": [] }],
                    "parameters": [
                        { "name": "session_id", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "responses": {
                        "200": {
                            "description": "Server-sent event stream",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/MessagesSnapshotResponse" }
                                }
                            }
                        },
                        "401": { "description": "OAuth bearer token required", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                        "404": { "description": "Session not found", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } }
                    }
                }
            },
            "/api/integrations/sessions/{session_id}/stop": {
                "post": {
                    "tags": ["Integrations"],
                    "summary": "Stop the active run for an integration session",
                    "security": [{ "bearerAuth": [] }],
                    "parameters": [
                        { "name": "session_id", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "responses": {
                        "204": { "description": "Stopped" },
                        "401": { "description": "OAuth bearer token required", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                        "404": { "description": "Session not found", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } }
                    }
                }
            },
            "/api/integrations/sessions/{session_id}/business-tool-requests/{request_id}/result": {
                "post": {
                    "tags": ["Integrations"],
                    "summary": "Submit the result for a business tool request",
                    "security": [{ "bearerAuth": [] }],
                    "parameters": [
                        { "name": "session_id", "in": "path", "required": true, "schema": { "type": "string" } },
                        { "name": "request_id", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/BusinessToolResultRequest" }
                            }
                        }
                    },
                    "responses": {
                        "200": { "description": "Existing result message reused", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/MessageResponse" } } } },
                        "201": { "description": "Created result message", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/MessageResponse" } } } },
                        "401": { "description": "OAuth bearer token required", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                        "404": { "description": "Request not found", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } },
                        "410": { "description": "Request expired", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorResponse" } } } }
                    }
                }
            }
        }
    })
}
