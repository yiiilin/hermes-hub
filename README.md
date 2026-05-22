# Hermes Hub

[![Release](https://github.com/yiiilin/hermes-hub/actions/workflows/release.yml/badge.svg)](https://github.com/yiiilin/hermes-hub/actions/workflows/release.yml)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Hermes Hub is a self-hosted console for running Hermes agents in isolated per-user Docker containers.

It provides invite-only accounts, model administration, chat sessions, file attachments, image generation, and an internal OpenAI-compatible gateway so provider keys stay inside the Hub.

[Docker Image](https://github.com/yiiilin/hermes-hub/pkgs/container/hermes-hub) | [Releases](https://github.com/yiiilin/hermes-hub/releases) | [Issues](https://github.com/yiiilin/hermes-hub/issues)

## Features

- Isolated Hermes container for every user.
- Invite-only registration with first-admin bootstrap.
- Chat workspace with sessions, attachments, tool progress, and image previews.
- Admin-managed chat, title, and image model configuration.
- Installable PWA experience for desktop and mobile browsers.
- Configurable per-user session limits.
- Internal LLM and channel gateways for managed Hermes containers.
- PostgreSQL persistence and S3-compatible object storage.
- Tag-based releases with GHCR image publishing.

## Quick Start

Requirements:

- Docker and Docker Compose
- An OpenAI-compatible model provider
- A host where mounting `/var/run/docker.sock` is acceptable

Create `.env` in the repository root:

```bash
HERMES_HUB_BACKEND_IMAGE=ghcr.io/yiiilin/hermes-hub:0.0.2
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

Start Hermes Hub:

```bash
docker compose --project-directory . --env-file .env -f infra/docker/docker-compose.hub.yml pull
docker compose --project-directory . --env-file .env -f infra/docker/docker-compose.hub.yml up -d --no-build
```

Open `http://localhost:8080`, create the first admin account, configure models, and create invite links for users.

## Build From Source

Requirements: Rust 1.88+, Node.js 24, npm, Docker, and Docker Compose.

```bash
make dev-db
cargo test --workspace

cd frontend
npm ci
npm test
npm run build
```

Run locally:

```bash
HERMES_HUB_STATIC_DIR=frontend/dist \
HERMES_DATA_ROOT="$(pwd)/data/hub/users" \
cargo run -p hermes-hub-backend
```

## Docker

Release images are published to GitHub Container Registry:

```bash
docker pull ghcr.io/yiiilin/hermes-hub:0.0.2
```

The image contains the Rust backend and the built React frontend. The service listens on port `8080`.

## Release

Releases are tag-driven. Pushing a tag such as `v0.0.2` triggers the release workflow, builds the Docker image, pushes GHCR tags, and creates a GitHub Release with the commit list.

## Security

Hermes Hub mounts the host Docker socket to create per-user Hermes containers. Run it only on infrastructure where Docker daemon access is intended.

Provider API keys are stored in Hub. Managed Hermes containers receive only internal gateway URLs and scoped instance tokens.

## License

Hermes Hub is licensed under the [MIT License](LICENSE).
