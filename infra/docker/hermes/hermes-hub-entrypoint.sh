#!/bin/bash
# Hermes Hub 托管容器入口：先把 Hub NFS 上的统一 profile 文件放到
# Hermes 会读取的位置，再交回官方 Hermes entrypoint 完成后续初始化。
set -e

HERMES_HUB_NFS_DIR="${HERMES_HUB_NFS_DIR:-/nfs}"

mkdir -p /config /workspace

# 官方 s6 stage2 会根据 /config 顶层 owner 决定是否递归修正安装目录权限。
# 这里先把 Hub 创建的挂载根目录交给 hermes，避免每次启动都扫描几百 MB 的运行时目录。
if id hermes >/dev/null 2>&1; then
    chown hermes:hermes /config /workspace 2>/dev/null || true
fi

for file in AGENTS.md SOUL.md; do
    ln -sfn "$HERMES_HUB_NFS_DIR/$file" "/config/$file"
    ln -sfn "$HERMES_HUB_NFS_DIR/$file" "/workspace/$file"
done

if [ -x /init ] && [ -x /opt/hermes/docker/main-wrapper.sh ]; then
    exec /init /opt/hermes/docker/main-wrapper.sh "$@"
fi

# 兼容旧版 Hermes 基础镜像：旧镜像没有 s6 /init 时仍然交给官方入口。
exec /opt/hermes/docker/entrypoint.sh "$@"
