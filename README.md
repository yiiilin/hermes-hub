# Hermes Hub

[![Release](https://github.com/yiiilin/hermes-hub/actions/workflows/release.yml/badge.svg)](https://github.com/yiiilin/hermes-hub/actions/workflows/release.yml)

Hermes Hub is an invite-only web console for running isolated Hermes agent containers for multiple users.

It provides authentication, admin-managed model settings, chat sessions, file attachments, and an internal OpenAI-compatible gateway so user containers never receive provider keys directly.

## Features

- Invite-only accounts with first-admin bootstrap.
- One managed Hermes container per user.
- React workspace for chat sessions, attachments, tool progress, and image previews.
- Rust/Axum API with PostgreSQL persistence and S3-compatible object storage.
- Admin model configuration for chat, title, and image generation models.
- Internal LLM and channel gateways for managed Hermes containers.
- Release images published to GitHub Container Registry.

## Quick Start

Prerequisites:

- Docker and Docker Compose
- A model provider compatible with OpenAI-style APIs
- A host where mounting `/var/run/docker.sock` is acceptable

Create `.env` in the repository root:

```bash
HERMES_HUB_BACKEND_IMAGE=ghcr.io/yiiilin/hermes-hub:0.0.1
HERMES_HUB_HTTP_PORT=8080

POSTGRES_PASSWORD=change-me
HERMES_HUB_SECRET_MASTER_KEY=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA

HERMES_HUB_MODEL_PROVIDER_BASE_URL=https://api.openai.com/v1
HERMES_HUB_MODEL_PROVIDER_API_KEY=sk-...
HERMES_HUB_DEFAULT_MODEL=gpt-4.1-mini
HERMES_HUB_ALLOWED_MODELS=gpt-4.1-mini
HERMES_HUB_MODEL_API_TYPE=responses

HERMES_OBJECT_STORAGE_ACCESS_KEY=rustfsadmin
HERMES_OBJECT_STORAGE_SECRET_KEY=change-me-rustfs-secret
```

Start the stack:

```bash
docker compose --project-directory . --env-file .env -f infra/docker/docker-compose.hub.yml pull
docker compose --project-directory . --env-file .env -f infra/docker/docker-compose.hub.yml up -d --no-build
```

Open `http://localhost:8080`, create the first admin account, configure models, then create invite links for users.

## Development

Development prerequisites: Rust 1.88 or newer, Node.js 24, npm, Docker, and Docker Compose.

```bash
# Start local dependencies.
make dev-db

# Backend tests.
cargo test --workspace

# Frontend tests and build.
cd frontend
npm ci
npm test
npm run build
```

Run the app locally:

```bash
HERMES_HUB_STATIC_DIR=frontend/dist \
HERMES_DATA_ROOT="$(pwd)/data/hub/users" \
cargo run -p hermes-hub-backend
```

## Docker Image

The release image contains the Rust backend and the built React frontend:

```bash
docker pull ghcr.io/yiiilin/hermes-hub:0.0.1
```

The backend serves the frontend from `/app/public` and listens on port `8080`.

## Releases

Releases are tag-driven. Pushing a tag such as `v0.0.1` triggers `.github/workflows/release.yml`.

The workflow:

- builds `infra/docker/backend.Dockerfile`
- pushes image tags to `ghcr.io/yiiilin/hermes-hub`
- creates or updates the GitHub Release
- writes the published image tags and commit list into the release notes

## Security Notes

Hermes Hub mounts the host Docker socket so it can create per-user Hermes containers. Treat the backend as a high-trust service and run it only on infrastructure where Docker daemon access is intended.

Provider API keys stay in Hub. Managed Hermes containers receive only an internal Hub gateway URL and an instance token.
