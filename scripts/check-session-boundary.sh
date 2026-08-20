#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
source_path="${SESSION_BOUNDARY_SOURCE:-$repo_root/crates/kitsu-core/src/session.rs}"

if [[ ! -f "$source_path" ]]; then
  echo "session boundary source is missing: $source_path" >&2
  exit 1
fi

# Canonical session values may serialize their own logical wire contract, but
# must not know how a host configures, locates, reads, writes, or presents a
# physical store. Store adapters and migrations remain outside kitsu-core.
for forbidden in \
  'crate::context' \
  'crate::orchestrator' \
  'kitsu_providers::' \
  'kitsu_render::' \
  'kitsu_tools::' \
  'serde_json::' \
  'std::env::' \
  'std::fs::' \
  'std::io::' \
  'std::path::' \
  'std::process::' \
  'std::time::' \
  'tokio::'
do
  if grep -Fn -- "$forbidden" "$source_path"; then
    echo "session boundary imports forbidden dependency: $forbidden" >&2
    exit 1
  fi
done

echo "session boundary is store-neutral: $source_path"
