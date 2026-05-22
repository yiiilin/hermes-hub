# Hermes Hub

Hermes Hub is an invite-only multi-user control plane for isolated Hermes agent instances.

## Current MVP

- Rust/Axum backend with invite-only auth, admin APIs, channel/session APIs, a real Hermes HTTP proxy, and a real OpenAI-compatible LLM proxy.
- React/Vite frontend with email/password login, first-admin bootstrap, invite registration, admin controls, channel workspace, session creation, and Hermes prompt dispatch.
- PostgreSQL/SQLx-backed runtime store for users, sessions, invites, channels, Hermes instances, model config, and instance tokens when `DATABASE_URL` is set.
- In-memory runtime mode remains available when `DATABASE_URL` is not set, mainly for lightweight tests and demos.
- Docker CLI based Hermes provisioner for one isolated Hermes container per user.
- Managed Hermes receives an internal OpenAI-compatible gateway URL plus an instance token; the browser API does not expose that token.
- Hub-owned session message snapshots and S3-compatible attachment storage, with RustFS wired in the Compose files.
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

For manual local app testing, build the frontend once and let the backend serve `frontend/dist`:

```bash
cd frontend
npm ci
npm run build
cd ..

cargo run -p hermes-hub-backend

# With PostgreSQL persistence enabled:
DATABASE_URL=postgres://hermes_hub:hermes_hub@127.0.0.1:5432/hermes_hub \
HERMES_HUB_SECRET_MASTER_KEY=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA \
HERMES_HUB_STATIC_DIR=frontend/dist \
HERMES_DATA_ROOT="$(pwd)/data/hub/users" \
HERMES_CONTAINER_NETWORK=hermes-hub-net \
HERMES_CONTAINER_CONNECT_MODE=published-host \
HERMES_HUB_LLM_BASE_URL=http://<host-ip>:8080/internal/llm/v1 \
cargo run -p hermes-hub-backend
```

You can still run `npm run dev` during UI development; that is only a local development convenience.

## Local Services

```bash
docker compose --project-directory . -f infra/docker/docker-compose.yml config
docker compose --project-directory . -f infra/docker/docker-compose.yml up -d postgres rustfs rustfs-init

# If local port 5432 is already in use:
POSTGRES_HOST_PORT=55432 docker compose --project-directory . -f infra/docker/docker-compose.yml up -d postgres

# Optional source-mounted dev servers.
docker compose --project-directory . -f infra/docker/docker-compose.yml --profile app-dev up backend frontend
```

The compose file is intentionally source-mounted for local development, not a production image build. The backend service mounts `/var/run/docker.sock` so it can create per-user Hermes containers on the `hermes-hub-net` network. That socket is a high-trust deployment boundary; in production, run the backend only where Docker daemon access is intended.

PostgreSQL data is bind-mounted under project root `./data/postgres`, RustFS object data is bind-mounted under `./data/rustfs`, and Hub-managed Hermes workspace/sandbox/config data is bind-mounted under `./data/hub/users`. The `data/` directory is ignored by git.

Managed Hermes containers use host-path workspace/sandbox/config directories under `HERMES_DATA_ROOT`. Hub creates the Docker network if needed, creates or starts a container named `hermes-user-<user-id>`, and injects:

- `OPENAI_BASE_URL=$HERMES_HUB_LLM_BASE_URL`
- `OPENAI_API_KEY=<instance token>`
- `OPENAI_MODEL=<admin default model>`

When Hub itself runs in Docker Compose, keep `HERMES_CONTAINER_CONNECT_MODE=network`; Hub reaches Hermes by container name and no host ports are published. When the backend runs directly on the host for local development, use `HERMES_CONTAINER_CONNECT_MODE=published-host`; each Hermes container publishes a random loopback port and Hub stores a local `base_url` such as `http://127.0.0.1:<port>`. In that host mode, `HERMES_HUB_LLM_BASE_URL` must be an address the Hermes container can use to call back into Hub, for example a LAN IP or Docker host-gateway address.

Hermes model traffic should target Hub’s internal `/internal/llm/v1` gateway. Admin model config changes take effect at the gateway without restarting Hermes containers.

### Files and Channel Delivery

Browser uploads go through Hub, are stored through the configured S3-compatible backend, and are returned as Hub attachment records with `/api/attachments/<id>/download` URLs. The first Compose target is RustFS, but the backend only depends on S3-compatible settings:

- `HERMES_OBJECT_STORAGE_ENDPOINT`
- `HERMES_OBJECT_STORAGE_BUCKET`
- `HERMES_OBJECT_STORAGE_REGION`
- `HERMES_OBJECT_STORAGE_ACCESS_KEY`
- `HERMES_OBJECT_STORAGE_SECRET_KEY`
- `HERMES_OBJECT_STORAGE_FORCE_PATH_STYLE`
- `HERMES_OBJECT_STORAGE_PREFIX`

Hermes-side channel adapters should use the internal protocol with the instance token that Hub already issues to each Hermes instance:

- `POST /internal/channel/v1/sessions/{session_id}/attachments` with `multipart/form-data`; Hub returns attachment metadata including `download_url`.
- `POST /internal/channel/v1/sessions/{session_id}/messages` with `{ "role": "assistant", "content": "文件：[name](download_url)", "attachments": [...] }`.

The message endpoint only accepts assistant messages from Hermes. Each attachment item must reference an attachment id returned by the upload endpoint for the same session; Hub rebuilds attachment metadata from its database and rejects unknown, cross-session, or wrong-direction attachment ids. Managed Hermes containers receive `HERMES_HUB_CHANNEL_BASE_URL` and `HERMES_HUB_CHANNEL_TOKEN` for this protocol. This keeps file lifecycle bound to the Hub session and avoids reading files directly from Hermes container bind mounts.

## Hub Deployment Compose

`infra/docker/docker-compose.hub.yml` is the deployment-oriented Compose file. It builds the React app during the backend image build, copies the static files into `/app/public`, and serves both API and frontend assets from the backend Web server. PostgreSQL stays on the internal Docker network, and only the backend HTTP port is exposed.

Create a `.env` file beside the compose command with at least:

```bash
POSTGRES_PASSWORD=change-me
HERMES_HUB_SECRET_MASTER_KEY=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA
HERMES_HUB_MODEL_PROVIDER_BASE_URL=https://api.openai.com/v1
HERMES_HUB_MODEL_PROVIDER_API_KEY=sk-...
HERMES_HUB_DEFAULT_MODEL=gpt-4.1-mini
HERMES_HUB_ALLOWED_MODELS=gpt-4.1-mini,gpt-4.1
HERMES_OBJECT_STORAGE_ACCESS_KEY=rustfsadmin
HERMES_OBJECT_STORAGE_SECRET_KEY=change-me-rustfs-secret
HERMES_HUB_HTTP_PORT=8080
```

Then deploy:

```bash
docker compose --project-directory . --env-file .env -f infra/docker/docker-compose.hub.yml config
docker compose --project-directory . --env-file .env -f infra/docker/docker-compose.hub.yml up -d --build
```

Run the compose commands from the project root. With `--project-directory .`, both compose files store PostgreSQL under `./data/postgres`, RustFS object data under `./data/rustfs`, and Hub/Hermes runtime data under `./data/hub/users`.

## Verification Boundary

Automated tests cover the Rust API, PostgreSQL persistence, fake Docker command lifecycle, real local HTTP proxying, frontend unit tests, browser smoke tests, and compose rendering. They do not pull or run the real `nousresearch/hermes-agent` image and do not call a paid model provider. Validate those two integrations manually with a real provider key before production use.

Remaining production hardening includes versioned migration rollout, rate limits, and deeper real-Hermes compatibility tests.

The project is implemented from the approved design in `docs/superpowers/specs/2026-05-19-hermes-hub-design.md`.
