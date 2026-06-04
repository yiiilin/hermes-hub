#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
用法:
  scripts/release.sh 0.0.23 "发版内容"
  scripts/release.sh 0.0.23 --notes-file release-notes.md

可选参数:
  --skip-verify   跳过 cargo/npm 验证，只更新版本、提交、打 tag
  --no-push       只在本地提交并打 tag，不推送
  --no-watch      推送后不等待 GitHub Actions release workflow

脚本会完成:
  1. 校验 main 分支、工作区干净、tag 不存在
  2. 更新 backend/frontend 版本和 lock 文件
  3. 运行后端测试、前端测试和前端构建
  4. 提交 chore: release <version>
  5. 创建 annotated tag，tag message 即发版内容
  6. 推送 main 和 tag，并等待 release workflow
EOF
}

log() {
  printf '[release] %s\n' "$*"
}

die() {
  printf '[release] ERROR: %s\n' "$*" >&2
  exit 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "缺少命令: $1"
}

if [ $# -eq 0 ]; then
  usage
  exit 1
fi

case "${1:-}" in
  -h|--help)
    usage
    exit 0
    ;;
esac

initial_dir="$(pwd)"
version_arg="$1"
shift

version="${version_arg#v}"
tag="v${version}"
notes_file=""
notes_text=()
skip_verify=false
no_push=false
no_watch=false

while [ $# -gt 0 ]; do
  case "$1" in
    --notes-file)
      shift
      [ $# -gt 0 ] || die "--notes-file 需要文件路径"
      notes_file="$1"
      shift
      ;;
    --skip-verify)
      skip_verify=true
      shift
      ;;
    --no-push)
      no_push=true
      shift
      ;;
    --no-watch)
      no_watch=true
      shift
      ;;
    --)
      shift
      while [ $# -gt 0 ]; do
        notes_text+=("$1")
        shift
      done
      ;;
    -*)
      die "未知参数: $1"
      ;;
    *)
      notes_text+=("$1")
      shift
      ;;
  esac
done

[[ "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "版本号必须是 x.y.z，例如 0.0.23"
[ -n "${notes_file}" ] || [ ${#notes_text[@]} -gt 0 ] || die "需要填写发版内容，或使用 --notes-file"
[ -z "${notes_file}" ] || [ ${#notes_text[@]} -eq 0 ] || die "--notes-file 和命令行发版内容只能选一种"

require_cmd git
require_cmd cargo
require_cmd npm
require_cmd python3

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || die "当前目录不在 git 仓库内"
cd "${repo_root}"

branch="$(git branch --show-current)"
[ "${branch}" = "main" ] || die "请在 main 分支发版，当前分支是 ${branch}"

notes_tmp="$(mktemp)"
trap 'rm -f "${notes_tmp}"' EXIT

allowed_untracked_path=""
if [ -n "${notes_file}" ]; then
  case "${notes_file}" in
    /*) ;;
    *) notes_file="${initial_dir}/${notes_file}" ;;
  esac
  [ -s "${notes_file}" ] || die "发版内容文件不存在或为空: ${notes_file}"
  cp "${notes_file}" "${notes_tmp}"
  allowed_untracked_path="$(
    python3 - "${repo_root}" "${notes_file}" <<'PY'
from pathlib import Path
import sys

repo = Path(sys.argv[1]).resolve()
notes = Path(sys.argv[2]).resolve()
try:
    print(notes.relative_to(repo).as_posix())
except ValueError:
    print("")
PY
  )"
else
  # 多个普通参数会按空格拼接；单个带换行的参数会保留原始换行。
  printf '%s\n' "${notes_text[*]}" > "${notes_tmp}"
fi

[ -s "${notes_tmp}" ] || die "发版内容不能为空"

dirty_status="$(
  ALLOWED_UNTRACKED_PATH="${allowed_untracked_path}" python3 - <<'PY'
import os
import subprocess

allowed = os.environ.get("ALLOWED_UNTRACKED_PATH", "")
raw = subprocess.check_output(["git", "status", "--porcelain=v1", "-z"])
entries = [entry for entry in raw.decode().split("\0") if entry]
dirty = []
for entry in entries:
    status = entry[:2]
    path = entry[3:]
    if status == "??" and allowed and path == allowed:
        continue
    dirty.append(entry)
print("\n".join(dirty))
PY
)"

if [ -n "${dirty_status}" ]; then
  printf '%s\n' "${dirty_status}" >&2
  die "工作区不干净；请先提交或暂存当前改动后再发版"
fi

log "同步远端 main 和 tags"
git fetch --tags origin
git pull --ff-only origin "${branch}"

git rev-parse -q --verify "refs/tags/${tag}" >/dev/null && die "tag 已存在: ${tag}"

log "更新版本到 ${version}"
python3 - "${version}" <<'PY'
from pathlib import Path
import re
import sys

version = sys.argv[1]
path = Path("backend/Cargo.toml")
text = path.read_text()
updated = re.sub(r'(?m)^(version = ")[^"]+(")$', rf'\g<1>{version}\2', text, count=1)
if updated == text:
    raise SystemExit("backend/Cargo.toml missing package version")
path.write_text(updated)
PY

cargo metadata --format-version 1 --no-deps >/dev/null

(
  cd frontend
  npm version "${version}" --no-git-tag-version >/dev/null
)

python3 - "${version}" <<'PY'
import json
from pathlib import Path
import sys

version = sys.argv[1]

backend = Path("backend/Cargo.toml").read_text()
if f'version = "{version}"' not in backend:
    raise SystemExit("backend version was not updated")

package = json.loads(Path("frontend/package.json").read_text())
lock = json.loads(Path("frontend/package-lock.json").read_text())
if package.get("version") != version:
    raise SystemExit("frontend package.json version was not updated")
if lock.get("version") != version:
    raise SystemExit("frontend package-lock.json root version was not updated")
if lock.get("packages", {}).get("", {}).get("version") != version:
    raise SystemExit("frontend package-lock.json package version was not updated")
PY

if [ "${skip_verify}" = false ]; then
  log "运行后端测试"
  cargo test -p hermes-hub-backend

  log "运行前端测试"
  (
    cd frontend
    npm test
  )

  log "构建前端"
  (
    cd frontend
    npm run build
  )

  log "检查 diff 空白"
  git diff --check
else
  log "已跳过验证"
fi

log "提交 release commit"
git add backend/Cargo.toml Cargo.lock frontend/package.json frontend/package-lock.json
git commit -m "chore: release ${version}"

log "创建 annotated tag ${tag}"
git tag -a "${tag}" -F "${notes_tmp}"

if [ "${no_push}" = true ]; then
  log "已按 --no-push 停在本地；需要时手动执行: git push origin ${branch} ${tag}"
  exit 0
fi

log "推送 ${branch}"
git push origin "${branch}"

log "推送 ${tag} 触发 release workflow"
git push origin "${tag}"

if [ "${no_watch}" = true ]; then
  log "已按 --no-watch 跳过等待"
  exit 0
fi

if ! command -v gh >/dev/null 2>&1; then
  log "未安装 gh，无法自动等待 workflow；请到 GitHub Actions 查看 ${tag}"
  exit 0
fi

log "等待 GitHub Actions release workflow"
run_id=""
for _ in $(seq 1 30); do
  runs_json="$(gh run list --workflow Release --limit 20 --json databaseId,headBranch,event 2>/dev/null || true)"
  run_id="$(
    RUNS_JSON="${runs_json}" TAG="${tag}" python3 - <<'PY'
import json
import os

try:
    runs = json.loads(os.environ.get("RUNS_JSON") or "[]")
except json.JSONDecodeError:
    runs = []

tag = os.environ["TAG"]
for run in runs:
    if run.get("headBranch") == tag and run.get("event") == "push":
        print(run.get("databaseId", ""))
        break
PY
  )"
  [ -n "${run_id}" ] && break
  sleep 3
done

if [ -z "${run_id}" ]; then
  log "未找到 ${tag} 对应的 workflow run；请到 GitHub Actions 手动查看"
  exit 0
fi

gh run watch "${run_id}" --exit-status
gh release view "${tag}"
