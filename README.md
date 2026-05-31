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

The production Compose file is `deploy/compose.prod.yml`. It is intentionally standalone: you can copy it and `deploy/.env.example` outside the repository and deploy from there without source code or build contexts.

Create a deployment directory and edit `.env`:

```bash
sudo mkdir -p /opt/hermes-hub
sudo cp deploy/compose.prod.yml /opt/hermes-hub/compose.yml
sudo cp deploy/.env.example /opt/hermes-hub/.env
cd /opt/hermes-hub
$EDITOR .env
```

At minimum, replace these values:

```bash
HERMES_HUB_BACKEND_IMAGE=ghcr.io/yiiilin/hermes-hub:latest
HERMES_DOCKER_IMAGE=ghcr.io/yiiilin/hermes-hub-hermes:latest
HERMES_HUB_HTTP_PORT=8080
HERMES_DATA_ROOT=/opt/hermes-hub/data/hub/users

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
docker compose --env-file .env -f compose.yml pull
docker compose --env-file .env -f compose.yml --profile hermes-runtime pull hermes-runtime
docker compose --env-file .env -f compose.yml up -d
```

`HERMES_DATA_ROOT` must be a host absolute path. The production Compose file bind-mounts that same path into the backend container because the backend talks to the host Docker daemon and creates sibling Hermes containers.

Only the Hub Web/API port is public by default. RustFS API, RustFS Console, and skills NFS are bound to `127.0.0.1`; Postgres is not published to the host.

Open `http://localhost:8080`, create the first admin account, configure models, and create invite links for users.

## Development Compose

The development Compose file is `deploy/compose.dev.yml`. It mounts the repository root into Rust and Node containers and keeps the existing `app-dev` profile workflow.

Start only the development database:

```bash
docker compose -f deploy/compose.dev.yml up -d postgres
```

Run the full local app stack from the repository root:

```bash
docker compose -f deploy/compose.dev.yml --profile app-dev up
```

The Vite frontend listens on `http://localhost:5173`, and the backend listens on `http://localhost:8080`.

## Build From Source

Requirements: Rust 1.88+, Node.js 24, npm, Docker, and Docker Compose.

```bash
docker compose -f deploy/compose.dev.yml up -d postgres
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
docker pull ghcr.io/yiiilin/hermes-hub:latest
docker pull ghcr.io/yiiilin/hermes-hub-hermes:latest
```

The Hub image contains the Rust backend and the built React frontend. The `hermes-hub-hermes` image is the Hermes runtime wrapper used by managed per-user containers. The service listens on port `8080`.

The Hermes runtime wrapper pins its upstream `nousresearch/hermes-agent:v2026.5.29.2` base image by digest. Do not rely on `latest` drift for routine Hub releases; update the Dockerfile `HERMES_AGENT_IMAGE` tag and digest only when intentionally aligning to a newer Hermes Agent base.

## Release

Releases are tag-driven. Pushing a release tag triggers the release workflow, builds the Docker image, pushes GHCR tags, and creates a GitHub Release with the commit list.

## Security

Hermes Hub mounts the host Docker socket to create per-user Hermes containers. Run it only on infrastructure where Docker daemon access is intended.

Provider API keys are stored in Hub. Managed Hermes containers receive only internal gateway URLs and scoped instance tokens.

## License

Hermes Hub is licensed under the [MIT License](LICENSE).
