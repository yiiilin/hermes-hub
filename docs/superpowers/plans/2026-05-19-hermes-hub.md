# Hermes Hub Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build an invite-only multi-user Hermes management platform with one isolated Hermes instance per user, channel-first workspaces, transparent Hermes proxying, and an internal OpenAI-compatible LLM gateway.

**Architecture:** Rust/Axum/Tokio backend, React/Vite frontend, PostgreSQL persistence, Docker-based managed Hermes provisioning, and a Hub-owned LLM proxy that Hermes instances use for all provider calls.

**Tech Stack:** Rust, Axum, Tokio, SQLx, PostgreSQL, React, Vite, TypeScript, Docker, Playwright.

**Spec:** `docs/superpowers/specs/2026-05-19-hermes-hub-design.md`

---

## Execution Rule

Progress must be tracked in this file only:

- Start every pending task as unchecked
- Mark each task with `[x]` immediately after implementation and verification complete
- If a task reveals a prerequisite gap, add a new unchecked task directly below it before continuing
- If any task remains unchecked, the project is not complete

## File Map

- `backend/` owns all Rust server code, migrations, tests, and Docker provisioning logic.
- `frontend/` owns the React workspace UI and admin UI.
- `infra/` owns Docker and local deployment files.
- `docs/` owns the spec and this plan.

### Task 1: Scaffold the repository and local developer workflow

**Files:**
- Create: `Cargo.toml`
- Create: `backend/Cargo.toml`
- Create: `backend/src/main.rs`
- Create: `backend/src/lib.rs`
- Create: `frontend/package.json`
- Create: `frontend/index.html`
- Create: `frontend/src/main.tsx`
- Create: `frontend/src/app.tsx`
- Create: `frontend/vite.config.ts`
- Create: `infra/docker/docker-compose.yml`
- Create: `Makefile`
- Create: `.env.example`
- Create: `README.md`

- [x] **Step 1: Write the failing smoke tests**

Add a backend test that fails until the app can load config and build the router, and add a frontend smoke test that fails until the React entry point renders.

- [x] **Step 2: Verify the repository does not build yet**

Run: `cargo test --workspace`

Expected: fail because the workspace and app code are not implemented yet.

- [x] **Step 3: Create the minimal workspace and app skeleton**

Add the root Rust workspace, backend binary entry point, frontend Vite entry point, Docker Compose file, and baseline docs/config files.

- [x] **Step 4: Verify the skeleton builds**

Run: `cargo test --workspace && cd frontend && npm test`

Expected: both commands pass after the minimal scaffolding exists.

- [x] **Step 5: Commit the scaffold**

```bash
git add Cargo.toml backend frontend infra Makefile .env.example README.md
git commit -m "chore: scaffold hermes hub workspace"
```

### Task 2: Add PostgreSQL schema, migrations, and encrypted secret storage

**Files:**
- Create: `backend/migrations/0001_init.sql`
- Create: `backend/src/db/mod.rs`
- Create: `backend/src/db/migrations.rs`
- Create: `backend/src/security/crypto.rs`
- Create: `backend/src/security/mod.rs`
- Create: `backend/tests/schema_test.rs`

- [x] **Step 1: Write migration tests first**

Add tests that fail until the initial tables exist and secret encryption/decryption is wired.

- [x] **Step 2: Verify schema tests fail**

Run: `cargo test -p hermes-hub-backend --test schema_test`

Expected: fail because the migration and crypto modules are not implemented yet.

- [x] **Step 3: Implement the initial tables and encrypted secret helpers**

Add the tables for users, sessions, invites, invite uses, channels, channel sessions, hermes instances, instance tokens, model configs, proxy audit logs, and llm usage events.

- [x] **Step 4: Verify migrations apply cleanly**

Run: `cargo test -p hermes-hub-backend --test schema_test`

Expected: pass.

- [x] **Step 5: Commit the schema work**

```bash
git add backend/migrations backend/src/db backend/src/security backend/tests/schema_test.rs
git commit -m "feat: add database schema and secret encryption"
```

### Task 3: Implement auth, bootstrap admin, and invite-only registration

**Files:**
- Create: `backend/src/http/auth.rs`
- Create: `backend/src/http/invites.rs`
- Create: `backend/src/http/mod.rs`
- Create: `backend/src/domain/user.rs`
- Create: `backend/src/domain/invite.rs`
- Create: `backend/src/session/store.rs`
- Create: `backend/tests/auth_invites_test.rs`

- [x] **Step 1: Write auth and invite tests first**

Add tests for first-user bootstrap admin creation, password hashing, login cookies, invite expiry, invite max uses, and invite revocation.

- [x] **Step 2: Verify the auth tests fail**

Run: `cargo test -p hermes-hub-backend --test auth_invites_test`

Expected: fail until the handlers and storage exist.

- [x] **Step 3: Implement the minimal auth and invite handlers**

Wire up bootstrap registration, invite creation, invite redemption, login, logout, and current-user lookup.

- [x] **Step 4: Verify invite-only registration works**

Run: `cargo test -p hermes-hub-backend --test auth_invites_test`

Expected: pass.

- [x] **Step 5: Commit auth and invite flow**

```bash
git add backend/src/http backend/src/domain backend/src/session backend/tests/auth_invites_test.rs
git commit -m "feat: add auth and invite flow"
```

### Task 4: Add Hermes instance registry and Docker provisioner

**Files:**
- Create: `backend/src/hermes/mod.rs`
- Create: `backend/src/hermes/instance.rs`
- Create: `backend/src/hermes/provisioner.rs`
- Create: `backend/src/hermes/docker_provisioner.rs`
- Create: `backend/src/hermes/health.rs`
- Create: `backend/tests/docker_provisioner_test.rs`
- Create: `infra/docker/hermes/Dockerfile`

- [x] **Step 1: Write provisioner tests first**

Add tests for ensure/start/stop/rebuild behavior, host-path generation, and container metadata without exposing host ports.

- [x] **Step 2: Verify the provisioner tests fail**

Run: `cargo test -p hermes-hub-backend --test docker_provisioner_test`

Expected: fail until the Docker adapter exists.

- [x] **Step 3: Implement the DockerProvisioner and HermesInstance persistence**

Create managed Hermes containers on an internal Docker network, mount per-user host directories, and persist container state in the database.

- [x] **Step 4: Verify managed Hermes lifecycle operations**

Run: `cargo test -p hermes-hub-backend --test docker_provisioner_test`

Expected: pass.

- [x] **Step 5: Commit the provisioning layer**

```bash
git add backend/src/hermes backend/tests/docker_provisioner_test.rs infra/docker/hermes/Dockerfile
git commit -m "feat: add docker hermes provisioner"
```

### Task 5: Implement the Hermes proxy and channel/session APIs

**Files:**
- Create: `backend/src/http/hermes_proxy.rs`
- Create: `backend/src/channel/mod.rs`
- Create: `backend/src/channel/service.rs`
- Create: `backend/src/channel/routes.rs`
- Create: `backend/tests/hermes_proxy_test.rs`

- [x] **Step 1: Write proxy and channel tests first**

Add tests for channel creation, session creation, Hermes path forwarding, streaming passthrough, and denylisted admin/internal routes.

- [x] **Step 2: Verify the proxy tests fail**

Run: `cargo test -p hermes-hub-backend --test hermes_proxy_test`

Expected: fail until the proxy and channel modules exist.

- [x] **Step 3: Implement the channel-first API and transparent Hermes proxy**

Add `channel -> session` persistence and forward user requests to the bound Hermes instance with request/response streaming preserved.

- [x] **Step 4: Verify session and proxy behavior**

Run: `cargo test -p hermes-hub-backend --test hermes_proxy_test`

Expected: pass.

- [x] **Step 5: Commit the Hermes proxy work**

```bash
git add backend/src/http backend/src/channel backend/tests/hermes_proxy_test.rs
git commit -m "feat: add hermes proxy and channel api"
```

### Task 6: Implement the Hub LLM gateway

**Files:**
- Create: `backend/src/http/llm_proxy.rs`
- Create: `backend/src/model_config.rs`
- Create: `backend/src/model_registry.rs`
- Create: `backend/tests/llm_proxy_test.rs`

- [x] **Step 1: Write model proxy tests first**

Add tests for `/internal/llm/v1/chat/completions`, `/internal/llm/v1/responses`, `/internal/llm/v1/models`, allowlist validation, default model insertion, and streaming passthrough.

- [x] **Step 2: Verify the LLM proxy tests fail**

Run: `cargo test -p hermes-hub-backend --test llm_proxy_test`

Expected: fail until the gateway exists.

- [x] **Step 3: Implement the OpenAI-compatible proxy**

Forward Hermes model calls to the active provider config, enforce the allowlist, and record usage metadata without storing prompt bodies.

- [x] **Step 4: Verify provider rotation does not require Hermes restarts**

Run: `cargo test -p hermes-hub-backend --test llm_proxy_test`

Expected: pass.

- [x] **Step 5: Commit the gateway**

```bash
git add backend/src/http backend/src/model_config.rs backend/src/model_registry.rs backend/tests/llm_proxy_test.rs
git commit -m "feat: add llm proxy"
```

### Task 7: Add admin and workspace backend APIs

**Files:**
- Create: `backend/src/http/admin.rs`
- Create: `backend/src/http/workspace.rs`
- Create: `backend/tests/admin_workspace_test.rs`
- Modify: `backend/src/lib.rs`
- Modify: `backend/src/http/mod.rs`
- Modify: `backend/src/model_config.rs`
- Modify: `backend/src/model_registry.rs`
- Modify: `backend/src/session/store.rs`
- Modify: `backend/src/hermes/docker_provisioner.rs`

- [x] **Step 1: Write backend admin/workspace tests first**

Add tests for model config read/update, user list, user disable/enable, Hermes instance listing, workspace ensure-hermes, and returning the current user Hermes instance.

- [x] **Step 2: Verify the admin/workspace tests fail**

Run: `cargo test -p hermes-hub-backend --test admin_workspace_test`

Expected: fail until the admin and workspace routes exist.

- [x] **Step 3: Implement the minimal admin and workspace backend APIs**

Wire model config management, user management, Hermes instance listing, external instance binding, and managed Hermes ensure/rebuild/start/stop endpoints.

- [x] **Step 4: Verify admin/workspace behavior**

Run: `cargo test -p hermes-hub-backend --test admin_workspace_test`

Expected: pass.

- [x] **Step 5: Commit the backend API layer**

```bash
git add backend/src/http backend/src/model_config.rs backend/src/model_registry.rs backend/src/session backend/src/hermes backend/tests/admin_workspace_test.rs
git commit -m "feat: add admin and workspace backend apis"
```

### Task 8: Build the React admin console and channel workspace

**Files:**
- Create: `frontend/src/api/client.ts`
- Create: `frontend/src/routes/login.tsx`
- Create: `frontend/src/routes/admin.tsx`
- Create: `frontend/src/routes/channels.tsx`
- Create: `frontend/src/routes/channel-session.tsx`
- Create: `frontend/src/components/layout.tsx`
- Create: `frontend/src/components/session-stream.tsx`
- Create: `frontend/e2e/auth.spec.ts`
- Create: `frontend/e2e/workspace.spec.ts`

- [x] **Step 1: Write frontend route and E2E tests first**

Add tests for login, bootstrap admin, invite redemption, channel creation, session streaming, and admin model configuration.

- [x] **Step 2: Verify the UI tests fail**

Run: `cd frontend && npm run test:e2e`

Expected: fail until the routes and API client exist.

- [x] **Step 3: Implement the minimal admin and workspace UI**

Build the login page, invite redemption flow, admin pages, channel list, session view, and streaming message area.

- [x] **Step 4: Verify the React app works end-to-end**

Run: `cd frontend && npm test && npm run test:e2e`

Expected: pass.

- [x] **Step 5: Commit the UI**

```bash
git add frontend/src frontend/e2e
git commit -m "feat: add admin console and workspace ui"
```

### Task 9: Add full integration coverage, docs, and release hardening

**Files:**
- Create: `backend/tests/integration.rs`
- Create: `.github/workflows/ci.yml`
- Update: `README.md`
- Update: `docs/` as needed

- [x] **Step 1: Write the integration and CI tests first**

Add a full-stack integration suite that covers bootstrap admin, invite-only signup, managed Hermes provisioning, proxy routing, and LLM gateway rotation.

- [x] **Step 2: Verify the full-stack tests fail before the final wiring is complete**

Run: `cargo test --workspace && cd frontend && npm test && cd .. && docker compose -f infra/docker/docker-compose.yml up --build`

Expected: fail until all modules are wired together.

- [x] **Step 3: Wire the last integration gaps and CI workflow**

Connect the backend, frontend, database, Docker provisioning, and deployment workflow into a single repeatable local stack.

- [x] **Step 4: Verify the release candidate passes the full suite**

Run: `cargo test --workspace && cd frontend && npm test && npm run test:e2e`

Expected: pass.

- [x] **Step 5: Commit the release hardening**

```bash
git add backend frontend infra .github README.md docs
git commit -m "chore: harden hermes hub release"
```

### Task 10: Replace mock runtime adapters with real production adapters

**Files:**
- Modify: `backend/Cargo.toml`
- Modify: `backend/src/lib.rs`
- Modify: `backend/src/app_config.rs`
- Modify: `backend/src/llm_proxy.rs`
- Modify: `backend/src/http/llm_proxy.rs`
- Modify: `backend/src/hermes/proxy_client.rs`
- Modify: `backend/src/http/hermes_proxy.rs`
- Modify: `backend/src/hermes/docker_provisioner.rs`
- Modify: `backend/src/http/workspace.rs`
- Modify: `backend/src/http/admin.rs`
- Modify: `backend/tests/llm_proxy_test.rs`
- Modify: `backend/tests/hermes_proxy_test.rs`
- Modify: `backend/tests/docker_provisioner_test.rs`

- [x] **Step 1: Add failing adapter tests first**

Add tests that prove production LLM/Hermes proxy clients hit real HTTP endpoints, preserve streaming-compatible headers, map upstream failures correctly, and that managed Docker provisioning can invoke Docker through a swappable command runner.

- [x] **Step 2: Implement real HTTP proxy adapters**

Add production reqwest-backed clients for Hermes and OpenAI-compatible providers, keep in-memory clients only for tests, enforce timeout/body limits, and avoid leaking provider/Hermes tokens through browser responses.

- [x] **Step 3: Implement real Docker daemon provisioning**

Create host directories, ensure the Docker network exists, create/start/stop/remove managed Hermes containers, inject Hub LLM proxy config, keep host ports unpublished, and persist container ids/status through existing APIs.

- [x] **Step 4: Verify backend adapter coverage**

Run: `cargo test --workspace`

Expected: pass with real adapters tested through local fake HTTP servers and fake Docker command runners, without requiring a real Hermes or Docker daemon in unit tests.

### Task 11: Close product hardening gaps for the first runnable release

**Files:**
- Modify: `backend/src/session/store.rs`
- Modify: `backend/src/http/llm_proxy.rs`
- Modify: `backend/src/http/hermes_proxy.rs`
- Modify: `frontend/src/api/client.ts`
- Modify: `frontend/src/routes/login.tsx`
- Modify: `frontend/src/routes/admin.tsx`
- Modify: `frontend/src/routes/channels.tsx`
- Modify: `frontend/src/routes/channel-session.tsx`
- Modify: `README.md`
- Modify: `.env.example`
- Modify: `infra/docker/docker-compose.yml`

- [x] **Step 1: Add audit/usage and UI behavior tests**

Add focused tests for proxy audit logs, LLM usage events, invite/login/admin/channel user flows, and encoded denied Hermes paths.

- [x] **Step 2: Implement audit/usage writes and frontend actions**

Record proxy/LLM metadata without prompt bodies, make login/register/invite/admin/model/channel/session controls actually call APIs, and surface usable loading/error states.

- [x] **Step 3: Update deployment docs and compose configuration**

Document production adapter defaults, Docker socket/data-root/network requirements, secret-key generation, and the limits of automated verification without a real Hermes image/provider key.

- [x] **Step 4: Verify the release candidate again**

Run: `cargo test --workspace && cd frontend && npm test && npm run build && npm run test:e2e && cd .. && docker compose -f infra/docker/docker-compose.yml config`

Expected: pass, with any real-Hermes/manual-provider gaps explicitly documented.

### Task 12: Add Hub-owned chat history, S3-backed attachments, and channel delivery protocol

**Files:**
- Modify: `backend/Cargo.toml`
- Modify: `Cargo.toml`
- Modify: `backend/migrations/0001_init.sql`
- Modify: `backend/src/app_config.rs`
- Modify: `backend/src/lib.rs`
- Modify: `backend/src/http/mod.rs`
- Modify: `backend/src/channel/routes.rs`
- Modify: `backend/src/channel/service.rs`
- Create: `backend/src/storage.rs`
- Create: `backend/src/http/attachments.rs`
- Create: `backend/src/http/channel_protocol.rs`
- Modify: `backend/tests/hermes_proxy_test.rs`
- Modify: `backend/tests/postgres_persistence_test.rs`
- Modify: `backend/tests/schema_test.rs`
- Modify: `frontend/src/api/client.ts`
- Modify: `frontend/src/components/layout.tsx`
- Modify: `frontend/src/routes/channel-session.tsx`
- Modify: `frontend/src/app.tsx`
- Modify: `frontend/src/styles.css`
- Modify: `frontend/src/app.test.tsx`
- Modify: `frontend/e2e/workspace.spec.ts`
- Modify: `infra/docker/docker-compose.yml`
- Modify: `infra/docker/docker-compose.hub.yml`
- Modify: `.env.example`
- Modify: `README.md`

- [x] **Step 1: Write failing backend tests for message history and attachment protocol**

Add tests for persisted session messages, object-backed attachment upload/download, and instance-token-authenticated channel delivery from Hermes into a Hub session.

Run: `cargo test -p hermes-hub-backend --test hermes_proxy_test --test postgres_persistence_test --test schema_test`

Expected: fail until storage, attachment routes, and channel protocol routes are implemented.

- [x] **Step 2: Implement storage, attachment metadata, and channel protocol routes**

Add a provider-neutral `ObjectStorage` abstraction with in-memory and S3-compatible implementations, store attachment metadata in PostgreSQL, expose browser upload/download routes, and expose internal Hermes channel routes authenticated by existing instance tokens.

Run: `cargo test -p hermes-hub-backend --test hermes_proxy_test --test postgres_persistence_test --test schema_test`

Expected: pass.

- [x] **Step 3: Write failing frontend tests for sidebar sessions, history loading, and attachments**

Add React and Playwright tests proving the global sidebar contains `New chat` and session rows, selecting a session loads stored messages, the chat pane scrolls instead of expanding the page, and uploaded attachments render from Hub download URLs.

Run: `cd frontend && npm test -- --run && npm run test:e2e`

Expected: fail until the UI and API client use the new routes.

- [x] **Step 4: Implement the frontend chat layout and API integration**

Move session navigation into the global sidebar via a chat slot, remove the inner chat sidebar, load/persist messages through Hub APIs, upload attachments before sending, and keep the right pane as a fixed-height scrollable chat surface.

Run: `cd frontend && npm test -- --run && npm run build && npm run test:e2e`

Expected: pass.

- [x] **Step 5: Add RustFS deployment wiring and release documentation**

Add RustFS plus bucket initialization to both Compose files under `./data/rustfs`, document required S3-compatible environment variables, and record Hermes channel/file protocol expectations.

Run: `docker compose -f infra/docker/docker-compose.yml config && docker compose --project-directory . -f infra/docker/docker-compose.hub.yml config`

Expected: pass.

- [x] **Step 6: Verify the full project and commit**

Run: `cargo test --workspace && cd frontend && npm test -- --run && npm run build && npm run test:e2e && cd .. && git diff --check`

Expected: pass. Then commit all Task 12 changes.

### Task 13: Replace prompt-based delivery with a native Hermes-Hub platform adapter

**Files:**
- Modify: `backend/migrations/0001_init.sql`
- Modify: `backend/src/channel/service.rs`
- Modify: `backend/src/channel/routes.rs`
- Modify: `backend/src/http/channel_protocol.rs`
- Modify: `backend/src/http/attachments.rs`
- Modify: `backend/src/hermes/docker_provisioner.rs`
- Modify: `backend/tests/hermes_proxy_test.rs`
- Modify: `backend/tests/docker_provisioner_test.rs`
- Modify: `backend/tests/schema_test.rs`
- Modify: `frontend/src/api/client.ts`
- Modify: `frontend/src/routes/channel-session.tsx`
- Modify: `frontend/src/app.test.tsx`
- Modify: `frontend/e2e/workspace.spec.ts`

- [x] **Step 1: Add failing backend tests for adapter queue protocol**

Add coverage for Hub-owned `channel_runs`, public enqueue, internal adapter long-poll/ack/status, internal input attachment download, and assistant message idempotency through `client_message_key`.

Run: `cargo test -p hermes-hub-backend --test hermes_proxy_test --test schema_test`

Expected: fail until the queue/status protocol exists.

- [x] **Step 2: Implement Hub queue, status, and internal protocol**

Persist each user turn as a Hub run, expose adapter long-poll and status endpoints authenticated by instance token, bind internal downloads to instance-owned sessions, and keep assistant delivery idempotent.

Run: `cargo test -p hermes-hub-backend --test hermes_proxy_test --test schema_test`

Expected: pass.

- [x] **Step 3: Generate and enable the Hermes `hermes_hub` adapter plugin**

Write `plugins/platforms/hermes_hub` into every managed Hermes config directory, enable it in `config.yaml`, and bump the Docker managed spec label so existing containers are recreated.

Run: `cargo test -p hermes-hub-backend --test docker_provisioner_test`

Expected: pass.

- [x] **Step 4: Move the frontend send path to Hub adapter runs**

Replace direct `/api/hermes/v1/runs` submission with `POST /api/channels/{channel_id}/sessions/{session_id}/runs`, poll Hub messages/run status for live updates, and stop appending duplicate final assistant messages in the browser.

Run: `cd frontend && npm test -- --run && npm run build`

Expected: pass.

- [x] **Step 5: Verify full adapter delivery path**

Run the backend suite, frontend suite, compose config check, and diff hygiene.

Run: `cargo test --workspace && cd frontend && npm test -- --run && npm run build && cd .. && docker compose -f infra/docker/docker-compose.yml config && git diff --check`

Expected: pass.
