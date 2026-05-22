# Hermes Hub Design Spec

## Overview
Hermes Hub is a multi-user control plane for the `NousResearch/hermes-agent` runtime.
It provides invite-only access, user and instance management, transparent Hermes API proxying, and an internal OpenAI-compatible LLM gateway.

The Hub is not a reimplementation of Hermes. Hermes keeps the agent runtime, tool execution, session handling, and model-facing behavior. The Hub owns identity, tenancy, provisioning, routing, auditing, and centralized model configuration.

## Goals
- Invite-only multi-user access.
- First registered user becomes `admin`.
- Each user gets one isolated Hermes instance.
- Support both `external` Hermes and `managed_docker` Hermes.
- Expose a channel-first Hub UI with multiple sessions per channel.
- Proxy Hermes APIs through the Hub instead of exposing Hermes directly to browsers.
- Proxy all model traffic through a Hub-owned OpenAI-compatible LLM gateway.
- Keep provider secrets inside the Hub, encrypted at rest.

## Non-Goals
- Kubernetes in v1.
- Email verification in v1.
- A new agent runtime that replaces Hermes.
- Direct browser access to Hermes instances.
- Storing prompt or completion bodies by default.

## Target Hermes
The target runtime is `NousResearch/hermes-agent`.

Relevant documented surfaces include:
- OpenAI-compatible API server.
- `POST /v1/chat/completions`
- `POST /v1/responses`
- `POST /v1/runs`
- `GET /v1/runs/{run_id}/events`
- `POST /v1/runs/{run_id}/stop`
- `GET /v1/models`
- `GET /v1/capabilities`
- `GET /health`
- `GET /health/detailed`

Hermes also documents profile isolation and shared-channel behavior via `group_sessions_per_user`, which is useful background for future external deployments.

## Core Product Model
### User
Human account in the Hub.
- Roles: `admin` and `user`
- Login: email + password
- Passwords are hashed with Argon2id
- Login state uses secure HttpOnly cookies

### Invite
Invite-only registration entry point.
- Token is random and stored hashed
- `expires_at` is required
- `max_uses` is required
- `used_count` is tracked
- Invite links are not bound to an email address
- Default expiry is 24 hours
- Default max uses is 1

### Hermes Instance
One isolated Hermes deployment per user.
- Kinds: `external` and `managed_docker`
- One active instance per user in v1
- External instances are manually bound by an admin
- Managed instances are created by the Hub through Docker

### Channel
The primary user-facing workspace unit in the Hub.
- A channel may contain many sessions
- A channel is what users navigate to first
- Channels are Hub-owned, not Hermes-owned

### Session
A concrete interaction thread inside a channel.
- One channel can have multiple sessions
- A session may be chat-oriented or agent-oriented
- The Hub stores the mapping to Hermes response/run identifiers
- Chat-style sessions map cleanly to Hermes `conversation` and `previous_response_id`
- Agent-style sessions map to Hermes `run_id` and SSE run events
- The Hub stores UI message snapshots and attachment metadata so session history remains stable even when Hermes does not expose a stable history REST API

### Model Config
Admin-managed global model settings.
- Provider base URL
- Provider API key
- Allowed model list
- Default model
- Streaming flag
- Request timeout

## Architecture
The system is split into three trust boundaries:

1. Browser to Hub
2. Hub to Hermes instance
3. Hermes to LLM provider via Hub LLM proxy

The browser only talks to the Hub.
The Hub routes API requests to the correct Hermes instance and never exposes Hermes credentials to the browser.
Managed Hermes instances run in Docker with host-path mounts for workspace, sandbox, and config, and do not expose host ports.

The Hub also owns an OpenAI-compatible gateway at `/internal/llm/v1/*`. Hermes instances point their model base URL at that gateway, so provider key rotation and model changes take effect without restarting Hermes.

## Data Model
Minimal v1 tables:
- `users`
- `sessions`
- `invites`
- `invite_uses`
- `channels`
- `channel_sessions`
- `hermes_instances`
- `instance_tokens`
- `model_configs`
- `proxy_audit_logs`
- `llm_usage_events`
- `channel_session_messages`
- `channel_attachments`

Secrets are stored encrypted in PostgreSQL with an application-level master key supplied through environment variables.

## API Surface
### Auth
- `POST /api/auth/bootstrap-register`
- `POST /api/auth/login`
- `POST /api/auth/logout`
- `GET /api/auth/me`

### Invites
- `POST /api/invites`
- `GET /api/invites`
- `POST /api/invites/:id/revoke`

### Admin
- `GET /api/admin/users`
- `POST /api/admin/users/:id/disable`
- `POST /api/admin/users/:id/enable`
- `GET /api/admin/hermes-instances`
- `POST /api/admin/hermes-instances/external`
- `POST /api/admin/users/:user_id/hermes-instance/bind-external`
- `POST /api/admin/users/:user_id/hermes-instance/rebuild-managed`
- `POST /api/admin/users/:user_id/hermes-instance/stop`
- `POST /api/admin/users/:user_id/hermes-instance/start`
- `GET /api/admin/model-config`
- `PUT /api/admin/model-config`

### Workspace
- `GET /api/workspace/status`
- `POST /api/workspace/ensure-hermes`
- `GET /api/workspace/hermes-instance`

### Channel and Session
- `GET /api/channels`
- `POST /api/channels`
- `GET /api/channels/:channel_id`
- `GET /api/channels/:channel_id/sessions`
- `POST /api/channels/:channel_id/sessions`
- `GET /api/channels/:channel_id/sessions/:session_id`
- `GET /api/channels/:channel_id/sessions/:session_id/messages`
- `POST /api/channels/:channel_id/sessions/:session_id/messages`
- `POST /api/channels/:channel_id/sessions/:session_id/attachments`
- `GET /api/attachments/:attachment_id/download`

### Hermes Channel Protocol
- `POST /internal/channel/v1/sessions/:session_id/attachments`
- `POST /internal/channel/v1/sessions/:session_id/messages`

The internal channel protocol is authenticated with the Hermes instance token. Hermes-side adapters upload generated files/images first, then deliver assistant messages whose markdown content references the returned Hub `download_url` values. Message attachments must reference Hub attachment ids from the same session, and Hub rebuilds attachment metadata from persisted records instead of trusting request JSON. Hub never reads Hermes container mount paths during normal message delivery.

### Hermes Proxy
- `/api/hermes/{path...}`

The proxy forwards method, path, query, body, and relevant headers to the user-bound Hermes instance.
Default denylist entries include internal and admin-style paths.

### LLM Proxy
- `POST /internal/llm/v1/chat/completions`
- `POST /internal/llm/v1/responses`
- `GET /internal/llm/v1/models`

The proxy enforces the admin model allowlist and returns an error if the request asks for a disallowed model.

## Runtime and Deployment
### Managed Hermes
- Provisioned by the Hub through Docker
- Uses a host directory per user under `/data/hermes-hub/users/<user_id>/`
- Mounts workspace, sandbox, and config directories
- Runs on an internal Docker network only
- Does not expose a host port

### External Hermes
- Manually registered by an admin
- The Hub stores the base URL and encrypted credential reference
- Used as-is, but still accessed through the Hub proxy

### Hub
- Rust backend with Axum, Tokio, SQLx
- React frontend
- PostgreSQL
- S3-compatible object storage for session-bound attachments, with RustFS as the Compose default
- Docker socket access for v1 provisioning

## Security
- Passwords are Argon2id hashed.
- All browser sessions use secure HttpOnly cookies.
- Provider and Hermes secrets are encrypted before storage.
- Hermes credentials are never sent to the browser.
- Managed Hermes containers are isolated from the host network.
- The LLM gateway is the only component that sees real provider API keys.

## Testing and Acceptance
The v1 release is acceptable only if:
- The first user becomes admin.
- Invite-only registration works with required expiry and max uses.
- Each user gets one isolated Hermes instance.
- Managed Hermes can be provisioned by Docker.
- The browser can use channel/session UI.
- Hermes API calls are proxied through the Hub.
- Hermes LLM calls go through the Hub LLM gateway.
- Admin model changes take effect without restarting Hermes.
- No prompt/completion bodies are stored by default.
