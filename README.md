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

The Compose file is `deploy/compose.yml`. It is intentionally standalone: you can copy it and `deploy/.env.example` outside the repository and deploy from there without source code or build contexts.

Create a deployment directory and edit `.env`:

```bash
sudo mkdir -p /opt/hermes-hub
sudo cp deploy/compose.yml /opt/hermes-hub/compose.yml
sudo cp deploy/.env.example /opt/hermes-hub/.env
cd /opt/hermes-hub
$EDITOR .env
```

At minimum, replace these values:

```bash
HERMES_HUB_BACKEND_IMAGE=ghcr.io/yiiilin/hermes-hub:latest
HERMES_DOCKER_IMAGE=ghcr.io/yiiilin/hermes-hub-hermes:vYYYY.M.D.N
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
docker compose --env-file .env pull
docker compose --env-file .env --profile hermes-runtime pull hermes-runtime
docker compose --env-file .env up -d
```

`HERMES_DATA_ROOT` must be a host absolute path. The Compose file bind-mounts that same path into the backend container because the backend talks to the host Docker daemon and creates sibling Hermes containers.

Only the Hub Web/API port is public by default. RustFS API, RustFS Console, and skills NFS are bound to `127.0.0.1`; Postgres is not published to the host.

Open `http://localhost:8080`, create the first admin account, configure models, and create invite links for users.

## Optional ASR

Speech input is disabled by default and Hermes Hub does not require an ASR service. The Compose file contains an optional `asr` profile. To enable it, edit `.env` in the deployment directory:

```bash
cd /opt/hermes-hub
$EDITOR .env
```

In `.env`, uncomment the ASR profile line and turn on the backend deployment switch:

```env
COMPOSE_PROFILES=asr
HERMES_HUB_SPEECH_INPUT_ENABLED=true
```

The default ASR image is `ghcr.io/yiiilin/hermes-hub-asr:v2026.6.4.21`. It wraps `sherpa-onnx` streaming Paraformer and exposes a WebSocket stream at `ws://asr:9991/stream`. The browser sends 16 kHz PCM16 audio through the Hub backend proxy, so replacement ASR images must implement the same streaming message contract instead of an HTTP multipart transcription endpoint.

Start without ASR:

```bash
docker compose --env-file .env up -d
```

Start with ASR:

```bash
docker compose --env-file .env config
docker compose --env-file .env up -d
```

When ASR is enabled at deployment level, an administrator still needs to enable speech input in System Settings before users see the microphone control.

## Build From Source

Requirements: Rust 1.88+, Node.js 24, npm, Docker, and Docker Compose.

```bash
docker compose --env-file deploy/.env.example -f deploy/compose.yml up -d postgres rustfs
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
docker pull ghcr.io/yiiilin/hermes-hub-hermes:vYYYY.M.D
docker pull ghcr.io/yiiilin/hermes-hub-asr:vYYYY.M.D
```

The Hub image contains the Rust backend and the built React frontend. The `hermes-hub-hermes` image is the Hermes runtime wrapper used by managed per-user containers. The optional `hermes-hub-asr` image wraps sherpa-onnx streaming ASR for speech input. The service listens on port `8080`.

Hermes wrapper and ASR runtime images use release-generated date tags such as `v2026.6.4` for the first UTC release that day and `v2026.6.4.2` for the second release that day; the final number is the daily release count, not a patch version. Use the exact tag from the GitHub Release notes. The Hermes runtime wrapper uses the selected upstream `nousresearch/hermes-agent:v2026.5.29.2` base image tag. Do not rely on `latest` drift for runtime images; update the Dockerfile `HERMES_AGENT_IMAGE` tag only when intentionally aligning to a newer Hermes Agent base.

## Release

Use the release script so the local version bump, verification, commit, annotated tag, push, workflow wait, and GitHub Release publication stay consistent:

```bash
scripts/release.sh 0.0.23 "Fix admin Hermes managed skills write access"
```

For multi-line release notes, write a notes file and pass it to the script:

```bash
scripts/release.sh 0.0.23 --notes-file release-notes.md
```

The Makefile wrapper is equivalent:

```bash
make release VERSION=0.0.23 NOTES="Fix admin Hermes managed skills write access"
make release VERSION=0.0.23 NOTES_FILE=release-notes.md
```

The script requires a clean `main` branch. It updates `backend/Cargo.toml`, `Cargo.lock`, `frontend/package.json`, and `frontend/package-lock.json`, runs backend tests, frontend tests, and the frontend build, then creates an annotated `vX.Y.Z` tag. The tag message is copied into the GitHub Release notes.

Pushing the tag triggers the release workflow. Each image build is path-diffed against the previous release tag: Hub, Hermes wrapper, and ASR are only build/pushed when their own image inputs changed. Skipped images are marked in the GitHub Release notes and continue using the existing registry image.

## Security

Hermes Hub mounts the host Docker socket to create per-user Hermes containers. Run it only on infrastructure where Docker daemon access is intended.

Provider API keys are stored in Hub. Managed Hermes containers receive only internal gateway URLs and scoped instance tokens.

## License

Hermes Hub is licensed under the [MIT License](LICENSE).
