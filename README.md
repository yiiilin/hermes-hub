# Hermes Hub

Hermes Hub is an invite-only multi-user control plane for isolated Hermes agent instances.

## Current MVP

- Rust/Axum backend with invite-only auth, admin APIs, channel/session APIs, a real Hermes HTTP proxy, and a real OpenAI-compatible LLM proxy.
- React/Vite frontend with email/password login, first-admin bootstrap, invite registration, admin controls, channel workspace, session creation, and Hermes prompt dispatch.
- PostgreSQL/SQLx-backed runtime store for users, sessions, invites, channels, Hermes instances, model config, and instance tokens when `DATABASE_URL` is set.
- In-memory runtime mode remains available when `DATABASE_URL` is not set, mainly for lightweight tests and demos.
- Docker CLI based Hermes provisioner for one isolated Hermes container per user.
- Managed Hermes receives an internal OpenAI-compatible gateway URL plus an instance token; the browser API does not expose that token.
- Proxy audit logs and LLM usage metadata are written without storing prompt bodies.

## Development

Prerequisites: Rust 1.82 or newer, Node.js 24 with npm, Docker CLI, and Docker Compose.

```bash
# Start the local PostgreSQL container.
make dev-db

# Backend tests include in-memory coverage. Set HERMES_HUB_TEST_DATABASE_URL for real PostgreSQL persistence coverage.
cargo test --workspace
HERMES_HUB_TEST_DATABASE_URL=postgres://hermes_hub:hermes_hub@127.0.0.1:5432/hermes_hub cargo test -p hermes-hub-backend --test postgres_persistence_test -- --nocapture

# Frontend unit, build, and browser smoke tests.
cd frontend
npm ci
npm test
npm run build
npm run test:e2e
```

For manual local app testing, run the backend and frontend dev server separately:

```bash
cargo run -p hermes-hub-backend

# With PostgreSQL persistence enabled:
DATABASE_URL=postgres://hermes_hub:hermes_hub@127.0.0.1:5432/hermes_hub \
HERMES_HUB_SECRET_MASTER_KEY=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA \
HERMES_DATA_ROOT=/data/hermes-hub/users \
HERMES_CONTAINER_NETWORK=hermes-hub-net \
HERMES_HUB_LLM_BASE_URL=http://hermes-hub:8080/internal/llm/v1 \
cargo run -p hermes-hub-backend

cd frontend
npm run dev -- --host 0.0.0.0 --port 5173
```

## Local Services

```bash
docker compose -f infra/docker/docker-compose.yml config
docker compose -f infra/docker/docker-compose.yml up -d postgres

# If local port 5432 is already in use:
POSTGRES_HOST_PORT=55432 docker compose -f infra/docker/docker-compose.yml up -d postgres

# Optional source-mounted dev servers.
docker compose -f infra/docker/docker-compose.yml --profile app-dev up backend frontend
```

The compose file is intentionally source-mounted for local development, not a production image build. The backend service mounts `/var/run/docker.sock` so it can create per-user Hermes containers on the `hermes-hub-net` network. That socket is a high-trust deployment boundary; in production, run the backend only where Docker daemon access is intended.

Managed Hermes containers use host-path workspace/sandbox/config directories under `HERMES_DATA_ROOT`. Hub creates the Docker network if needed, creates or starts a container named `hermes-user-<user-id>`, does not publish host ports, and injects:

- `OPENAI_BASE_URL=$HERMES_HUB_LLM_BASE_URL`
- `OPENAI_API_KEY=<instance token>`
- `OPENAI_MODEL=<admin default model>`

Hermes model traffic should target Hub’s internal `/internal/llm/v1` gateway. Admin model config changes take effect at the gateway without restarting Hermes containers.

## CI

GitHub Actions renders the compose config, runs `cargo test --workspace`, runs the PostgreSQL persistence integration test against a service container, and runs the frontend `npm ci`, `npm test`, `npm run build`, and `npm run test:e2e` flow.

## Verification Boundary

Automated tests cover the Rust API, PostgreSQL persistence, fake Docker command lifecycle, real local HTTP proxying, frontend unit tests, browser smoke tests, and compose rendering. They do not pull or run the real `nousresearch/hermes-agent` image and do not call a paid model provider. Validate those two integrations manually with a real provider key before production use.

Remaining production hardening includes versioned migration rollout, production backend/frontend images, rate limits, and deeper real-Hermes compatibility tests.

The project is implemented from the approved design in `docs/superpowers/specs/2026-05-19-hermes-hub-design.md`.
