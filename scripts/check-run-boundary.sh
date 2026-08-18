#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
source_path="${RUN_BOUNDARY_SOURCE:-$repo_root/crates/kitsu-core/src/run.rs}"

if [[ ! -f "$source_path" ]]; then
  echo "run boundary source is missing: $source_path" >&2
  exit 1
fi

# The first run contract is intentionally a neutral value layer. Generation,
# input, presentation, persistence, provider/tool execution, and process exit
# remain owned by adapters around it.
for forbidden in \
  'crate::cli' \
  'crate::session' \
  'kitsu_providers::' \
  'kitsu_render::' \
  'kitsu_tools::' \
  'serde::' \
  'serde_json::' \
  'std::env::' \
  'std::fs::' \
  'std::io::' \
  'std::process::' \
  'std::time::' \
  'tokio::'
do
  if grep -Fn -- "$forbidden" "$source_path"; then
    echo "run boundary imports forbidden dependency: $forbidden" >&2
    exit 1
  fi
done

echo "run boundary is carrier-neutral: $source_path"
