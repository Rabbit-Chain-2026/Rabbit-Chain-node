#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
WORKSPACE_ROOT="${REDLINE_WORKSPACE_ROOT:-$(cd "${REPO_ROOT}/.." && pwd)}"
SCOPE="${REDLINE_SCOPE:-workspace}"

ALLOW_TAG="REDLINE_ALLOW"
FAILED=0

usage() {
  cat <<EOF
Usage:
  $(basename "$0") [--scope workspace|repo] [-d <dir>]...

Options:
  -d, --dir <dir>   指定要检查的项目目录（可重复传入）
  --scope <scope>   自动发现模式：workspace(默认) 或 repo
  -h, --help        显示帮助

Examples:
  $(basename "$0")
  $(basename "$0") --scope repo
  $(basename "$0") -d ../Rabbit-Chain-node -d ../rabbitchain-explorer
EOF
}

resolve_dir() {
  local raw="$1"
  if [[ ! -d "${raw}" ]]; then
    echo "invalid directory: ${raw}" >&2
    exit 2
  fi
  (cd "${raw}" && pwd)
}

discover_projects() {
  local root="$1"
  local -a projects=()
  local dir

  while IFS= read -r dir; do
    [[ -e "${dir}/.git" ]] || continue
    projects+=("${dir}")
  done < <(find "${root}" -mindepth 1 -maxdepth 1 -type d | sort)

  printf '%s\n' "${projects[@]}"
}

check_pattern() {
  local project_dir="$1"
  local title="$2"
  local pattern="$3"
  local matches filtered
  local project_name

  project_name="$(basename "${project_dir}")"

  pushd "${project_dir}" >/dev/null
  matches="$(rg -n -P --no-heading --color never "${pattern}" . \
    --glob '!**/tests/**' \
    --glob '!**/target/**' \
    --glob '!**/node_modules/**' \
    --glob '!**/dist/**' \
    --glob '!**/build/**' \
    --glob '!**/.dart_tool/**' \
    --glob '!**/coverage/**' \
    --glob '!**/artifacts/**' || true)"
  popd >/dev/null

  # normalize to path relative to workspace root for readable output
  matches="$(printf '%s\n' "${matches}" | sed "s#^./#${project_name}/#")"

  if [[ -z "${matches}" ]]; then
    return 0
  fi

  filtered="$(printf '%s\n' "${matches}" | grep -v "${ALLOW_TAG}" || true)"
  if [[ -z "${filtered}" ]]; then
    return 0
  fi

  FAILED=1
  echo "❌ [${project_name}] ${title}"
  printf '%s\n' "${filtered}"
  echo
}

declare -a INPUT_DIRS=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    -d|--dir)
      shift
      [[ $# -gt 0 ]] || { echo "missing value for --dir" >&2; exit 2; }
      INPUT_DIRS+=("$(resolve_dir "$1")")
      ;;
    --scope)
      shift
      [[ $# -gt 0 ]] || { echo "missing value for --scope" >&2; exit 2; }
      SCOPE="$1"
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      INPUT_DIRS+=("$(resolve_dir "$1")")
      ;;
  esac
  shift
done

if [[ "${SCOPE}" != "workspace" && "${SCOPE}" != "repo" ]]; then
  echo "invalid scope: ${SCOPE} (expected workspace|repo)" >&2
  exit 2
fi

echo "🔒 Engineering redline guard (no silent fallback)"
echo "scope=${SCOPE} workspace_root=${WORKSPACE_ROOT}"

declare -a PROJECTS=()
if [[ ${#INPUT_DIRS[@]} -gt 0 ]]; then
  PROJECTS=("${INPUT_DIRS[@]}")
elif [[ "${SCOPE}" == "repo" ]]; then
  PROJECTS=("$(resolve_dir "${REPO_ROOT}")")
else
  while IFS= read -r project; do
    [[ -n "${project}" ]] && PROJECTS+=("${project}")
  done < <(discover_projects "${WORKSPACE_ROOT}")
fi

if [[ ${#PROJECTS[@]} -eq 0 ]]; then
  echo "⚠️ no project discovered, fallback to current repo: ${REPO_ROOT}"
  PROJECTS=("${REPO_ROOT}")
fi

echo "projects=${#PROJECTS[@]}"
for project in "${PROJECTS[@]}"; do
  echo " - $(basename "${project}")"
done

for project in "${PROJECTS[@]}"; do
  check_pattern \
    "${project}" \
    "禁止配置错误后回退默认配置继续运行" \
    'Self::try_new\((ApiConfig|RpcConfig)::default\(\)\)'

  check_pattern \
    "${project}" \
    "禁止以 fallback 语义吞掉关键故障（default/mem）" \
    'fallback to (default|mem)'

  check_pattern \
    "${project}" \
    "禁止端口解析失败后默认 30303" \
    'parse\(\)\.unwrap_or\(\s*30303\s*\)'
done

if [[ "${FAILED}" -ne 0 ]]; then
  echo "🚫 Redline guard failed. Resolve or annotate with ${ALLOW_TAG} (must include rationale)."
  exit 1
fi

echo "✅ Redline guard passed"
