#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/tag_release.sh [options]

Options:
  --tag <name>        Use a custom tag name (default: vYYYY.MM.DD-HHMMUTC)
  --message <text>    Custom tag message (default: "Release <tag>")
  --dry-run           Show the commands without executing them
  -h, --help          Show this help message
USAGE
}

check_clean_worktree() {
  if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "Error: working tree has uncommitted changes." >&2
    exit 1
  fi
}

main() {
  local tag=""
  local message=""
  local dry_run=false

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --tag)
        shift || { echo "Missing value for --tag" >&2; exit 1; }
        tag="$1"
        ;;
      --tag=*)
        tag="${1#*=}"
        ;;
      --message)
        shift || { echo "Missing value for --message" >&2; exit 1; }
        message="$1"
        ;;
      --message=*)
        message="${1#*=}"
        ;;
      --dry-run)
        dry_run=true
        ;;
      -h|--help)
        usage
        exit 0
        ;;
      *)
        echo "Unknown option: $1" >&2
        usage >&2
        exit 1
        ;;
    esac
    shift || true
  done

  if [[ -z "$tag" ]]; then
    tag="v$(date -u +%Y.%m.%d-%H%MUTC)"
  fi
  if [[ -z "$message" ]]; then
    message="Release ${tag}"
  fi

  if git rev-parse "$tag" >/dev/null 2>&1; then
    echo "Error: tag '$tag' already exists locally." >&2
    exit 1
  fi

  if git ls-remote --tags origin "$tag" | grep -q "$tag"; then
    echo "Error: tag '$tag' already exists on origin." >&2
    exit 1
  fi

  if ! $dry_run; then
    check_clean_worktree
  fi

  echo "Tag: $tag"
  echo "Message: $message"

  if $dry_run; then
    echo "Dry run: git tag -a '$tag' -m '$message'"
    echo "Dry run: git push origin '$tag'"
    exit 0
  fi

  git tag -a "$tag" -m "$message"
  git push origin "$tag"

  echo "Created and pushed $tag"
}

main "$@"
