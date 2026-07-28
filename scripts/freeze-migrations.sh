#!/usr/bin/env bash
# Record the current migrations as the immutable baseline (migrations/.checksums).
# Run this ONLY when you intend the current migration bytes to be frozen forever,
# i.e. right before/at a release, or when deliberately re-baselining.
set -euo pipefail
cd "$(dirname "$0")/.."

if command -v sha256sum >/dev/null 2>&1; then
  SHA=sha256sum
elif command -v shasum >/dev/null 2>&1; then
  SHA="shasum -a 256"
else
  echo "ERROR: need sha256sum or shasum on PATH."; exit 2
fi

OUT="migrations/.checksums"
{
  echo "# Frozen migration checksums (sha256). Managed by scripts/freeze-migrations.sh."
  echo "# Do not edit by hand. An already-frozen migration must never change; add a"
  echo "# new migration file instead. See docs/DEPLOY.md."
  for f in $(ls migrations/*.sql | sort); do
    sum="$($SHA "$f" | awk '{print $1}')"
    printf '%s  %s\n' "$sum" "$f"
  done
} > "$OUT"

echo "Wrote $OUT:"
cat "$OUT"
