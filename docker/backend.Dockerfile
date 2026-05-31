FROM rust:1.88-bookworm AS builder

WORKDIR /app

# 先复制 manifest，利用 Docker 层缓存依赖编译结果。
COPY Cargo.toml Cargo.lock ./
COPY backend/Cargo.toml backend/Cargo.toml
COPY backend/src backend/src
COPY backend/migrations backend/migrations

RUN cargo build --release -p hermes-hub-backend --bins

FROM node:24-alpine AS frontend-builder

WORKDIR /app/frontend

# 前端构建产物最终由 backend 直接托管，不再需要单独的 nginx/frontend 容器。
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci

COPY frontend ./
RUN npm run build

FROM docker:29.1.3-cli AS docker-cli

FROM debian:bookworm-slim AS runtime

# backend 需要 Docker CLI 通过宿主机 Docker socket 创建用户级 Hermes 容器。
# Debian bookworm 的 docker.io 仍是 20.10.x，客户端 API 只有 1.41，
# 无法连接 Docker 29.x daemon；这里固定复制官方新版 CLI，避免远端 daemon 拒绝连接。
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=docker-cli /usr/local/bin/docker /usr/local/bin/docker
COPY --from=builder /app/target/release/hermes-hub-backend /usr/local/bin/hermes-hub-backend
COPY --from=builder /app/target/release/hermes-hub-fs /usr/local/bin/hermes-hub-fs
COPY --from=frontend-builder /app/frontend/dist /app/public

ENV HERMES_HUB_BIND_ADDR=0.0.0.0:8080
ENV HERMES_HUB_STATIC_DIR=/app/public

EXPOSE 8080 12049

CMD ["hermes-hub-backend"]
