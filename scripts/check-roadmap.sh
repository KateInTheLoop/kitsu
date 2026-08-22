#!/usr/bin/env bash
set -euo pipefail

roadmap_base="${ROADMAP_BASE:-a2ba2d5c98b17da7290a2b6b8b24c83bc2529763}"
pending=0
if [[ "${1:-}" == "--pending" ]]; then
  pending=1
elif [[ $# -ne 0 ]]; then
  echo "usage: $0 [--pending]" >&2
  exit 2
fi

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"
git merge-base --is-ancestor "$roadmap_base" HEAD

commit_count="$(git rev-list --count "$roadmap_base..HEAD")"
expected_count=$((commit_count + pending))
mapfile -t cycle_files < <(find docs/roadmap/cycles -maxdepth 1 -type f -name '*.md' -printf '%f\n' | sort)

if [[ "${#cycle_files[@]}" -ne "$expected_count" ]]; then
  echo "roadmap receipt mismatch: ${#cycle_files[@]} cycle files for $commit_count commits (pending=$pending)" >&2
  exit 1
fi

for ((index = 1; index <= expected_count; index += 1)); do
  cycle="$(printf '%03d' "$index")"
  path="docs/roadmap/cycles/$cycle.md"
  if [[ ! -f "$path" ]]; then
    echo "missing sequential cycle receipt: $path" >&2
    exit 1
  fi
  for marker in \
    '- Epic/story/card:' \
    '- Requirement:' \
    '- Tests added or changed:' \
    '- Review findings and fixes:' \
    '- Acceptance commands/results:' \
    '- Commit:' \
    '- Next card:'
  do
    if ! grep -Fq -- "$marker" "$path"; then
      echo "missing '$marker' in $path" >&2
      exit 1
    fi
  done
done

echo "roadmap receipts valid: ${#cycle_files[@]} cycles, $commit_count commits, pending=$pending"
