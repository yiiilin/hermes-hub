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

- [ ] **Step 1: Write the failing smoke tests**

Add a backend test that fails until the app can load config and build the router, and add a frontend smoke test that fails until the React entry point renders.

- [ ] **Step 2: Verify the repository does not build yet**

Run: `cargo test --workspace`

Expected: fail because the workspace and app code are not implemented yet.

- [ ] **Step 3: Create the minimal workspace and app skeleton**

Add the root Rust workspace, backend binary entry point, frontend Vite entry point, Docker Compose file, and baseline docs/config files.

- [ ] **Step 4: Verify the skeleton builds**

Run: `cargo test --workspace && cd frontend && npm test`

Expected: both commands pass after the minimal scaffolding exists.

- [ ] **Step 5: Commit the scaffold**

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

- [ ] **Step 1: Write migration tests first**

Add tests that fail until the initial tables exist and secret encryption/decryption is wired.

- [ ] **Step 2: Verify schema tests fail**

Run: `cargo test -p hermes-hub-backend schema_test`

Expected: fail because the migration and crypto modules are not implemented yet.

- [ ] **Step 3: Implement the initial tables and encrypted secret helpers**

Add the tables for users, sessions, invites, invite uses, channels, channel sessions, hermes instances, instance tokens, model configs, proxy audit logs, and llm usage events.

- [ ] **Step 4: Verify migrations apply cleanly**

Run: `cargo test -p hermes-hub-backend schema_test`

Expected: pass.

- [ ] **Step 5: Commit the schema work**

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

- [ ] **Step 1: Write auth and invite tests first**

Add tests for first-user bootstrap admin creation, password hashing, login cookies, invite expiry, invite max uses, and invite revocation.

- [ ] **Step 2: Verify the auth tests fail**

Run: `cargo test -p hermes-hub-backend auth_invites_test`

Expected: fail until the handlers and storage exist.

- [ ] **Step 3: Implement the minimal auth and invite handlers**

Wire up bootstrap registration, invite creation, invite redemption, login, logout, and current-user lookup.

- [ ] **Step 4: Verify invite-only registration works**

Run: `cargo test -p hermes-hub-backend auth_invites_test`

Expected: pass.

- [ ] **Step 5: Commit auth and invite flow**

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

- [ ] **Step 1: Write provisioner tests first**

Add tests for ensure/start/stop/rebuild behavior, host-path generation, and container metadata without exposing host ports.

- [ ] **Step 2: Verify the provisioner tests fail**

Run: `cargo test -p hermes-hub-backend docker_provisioner_test`

Expected: fail until the Docker adapter exists.

- [ ] **Step 3: Implement the DockerProvisioner and HermesInstance persistence**

Create managed Hermes containers on an internal Docker network, mount per-user host directories, and persist container state in the database.

- [ ] **Step 4: Verify managed Hermes lifecycle operations**

Run: `cargo test -p hermes-hub-backend docker_provisioner_test`

Expected: pass.

- [ ] **Step 5: Commit the provisioning layer**

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

- [ ] **Step 1: Write proxy and channel tests first**

Add tests for channel creation, session creation, Hermes path forwarding, streaming passthrough, and denylisted admin/internal routes.

- [ ] **Step 2: Verify the proxy tests fail**

Run: `cargo test -p hermes-hub-backend hermes_proxy_test`

Expected: fail until the proxy and channel modules exist.

- [ ] **Step 3: Implement the channel-first API and transparent Hermes proxy**

Add `channel -> session` persistence and forward user requests to the bound Hermes instance with request/response streaming preserved.

- [ ] **Step 4: Verify session and proxy behavior**

Run: `cargo test -p hermes-hub-backend hermes_proxy_test`

Expected: pass.

- [ ] **Step 5: Commit the Hermes proxy work**

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

- [ ] **Step 1: Write model proxy tests first**

Add tests for `/internal/llm/v1/chat/completions`, `/internal/llm/v1/responses`, `/internal/llm/v1/models`, allowlist validation, default model insertion, and streaming passthrough.

- [ ] **Step 2: Verify the LLM proxy tests fail**

Run: `cargo test -p hermes-hub-backend llm_proxy_test`

Expected: fail until the gateway exists.

- [ ] **Step 3: Implement the OpenAI-compatible proxy**

Forward Hermes model calls to the active provider config, enforce the allowlist, and record usage metadata without storing prompt bodies.

- [ ] **Step 4: Verify provider rotation does not require Hermes restarts**

Run: `cargo test -p hermes-hub-backend llm_proxy_test`

Expected: pass.

- [ ] **Step 5: Commit the gateway**

```bash
git add backend/src/http backend/src/model_config.rs backend/src/model_registry.rs backend/tests/llm_proxy_test.rs
git commit -m "feat: add llm proxy"
```

### Task 7: Build the React admin console and channel workspace

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

- [ ] **Step 1: Write frontend route and E2E tests first**

Add tests for login, bootstrap admin, invite redemption, channel creation, session streaming, and admin model configuration.

- [ ] **Step 2: Verify the UI tests fail**

Run: `cd frontend && npm run test:e2e`

Expected: fail until the routes and API client exist.

- [ ] **Step 3: Implement the minimal admin and workspace UI**

Build the login page, invite redemption flow, admin pages, channel list, session view, and streaming message area.

- [ ] **Step 4: Verify the React app works end-to-end**

Run: `cd frontend && npm test && npm run test:e2e`

Expected: pass.

- [ ] **Step 5: Commit the UI**

```bash
git add frontend/src frontend/e2e
git commit -m "feat: add admin console and workspace ui"
```

### Task 8: Add full integration coverage, docs, and release hardening

**Files:**
- Create: `backend/tests/integration.rs`
- Create: `.github/workflows/ci.yml`
- Update: `README.md`
- Update: `docs/` as needed

- [ ] **Step 1: Write the integration and CI tests first**

Add a full-stack integration suite that covers bootstrap admin, invite-only signup, managed Hermes provisioning, proxy routing, and LLM gateway rotation.

- [ ] **Step 2: Verify the full-stack tests fail before the final wiring is complete**

Run: `cargo test --workspace && cd frontend && npm test && cd .. && docker compose -f infra/docker/docker-compose.yml up --build`

Expected: fail until all modules are wired together.

- [ ] **Step 3: Wire the last integration gaps and CI workflow**

Connect the backend, frontend, database, Docker provisioning, and deployment workflow into a single repeatable local stack.

- [ ] **Step 4: Verify the release candidate passes the full suite**

Run: `cargo test --workspace && cd frontend && npm test && npm run test:e2e`

Expected: pass.

- [ ] **Step 5: Commit the release hardening**

```bash
git add backend frontend infra .github README.md docs
git commit -m "chore: harden hermes hub release"
```
