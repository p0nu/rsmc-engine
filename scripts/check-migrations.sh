#!/usr/bin/env bash
# ============================================================================
# Migration immutability guard
# ============================================================================
# Enforces the golden rule: once a migration has shipped, its bytes never
# change. Editing an already-applied migration changes its SQLx checksum and
# breaks startup on every existing database ("migration N was previously
# applied but has been modified"). All schema changes must go in a NEW,
# higher-numbered migration file instead.
#
# How it works: migrations/.checksums is a committed manifest of
# "<sha256>  <filename>" for every FROZEN migration. This script recomputes the
# checksums of the files on disk and fails if a frozen file changed or went
# missing. New migration files that are not yet in the manifest are allowed
# (they are still in development); freeze them with `make freeze-migrations`
# once they ship.
#
# Exit codes: 0 = OK, 1 = a frozen migration changed/was deleted.

set -euo pipefail

cd "$(dirname "$0")/.."   # repo root (scripts/ is one level down)

MANIFEST="migrations/.checksums"

if [ ! -f "$MANIFEST" ]; then
  echo "No $MANIFEST found. Create it with: make freeze-migrations"
  echo "(This records the current migrations as the immutable baseline.)"
  exit 1
fi

# Pick a sha256 tool that exists on this platform.
if command -v sha256sum >/dev/null 2>&1; then
  sha() { sha256sum "$1" | awk '{print $1}'; }
elif command -v shasum >/dev/null 2>&1; then
  sha() { shasum -a 256 "$1" | awk '{print $1}'; }
else
  echo "ERROR: need sha256sum or shasum on PATH."
  exit 2
fi

violations=0

# Check every frozen entry: file must exist and match its recorded checksum.
while read -r want file; do
  [ -z "${want:-}" ] && continue
  case "$want" in \#*) continue ;; esac   # allow comment lines
  if [ ! -f "$file" ]; then
    echo "VIOLATION: frozen migration '$file' is missing (was it deleted or renamed?)"
    violations=$((violations + 1))
    continue
  fi
  got="$(sha "$file")"
  if [ "$got" != "$want" ]; then
    echo "VIOLATION: '$file' changed since it was frozen."
    echo "    expected $want"
    echo "    found    $got"
    violations=$((violations + 1))
  fi
done < "$MANIFEST"

if [ "$violations" -gt 0 ]; then
  cat <<'MSG'

ERROR: one or more frozen migrations were modified.

Migrations are immutable once shipped. Do NOT edit an existing migration,
not even a comment or whitespace: it changes the file's checksum and breaks
startup on every database that already applied it.

To make a schema change, add a NEW migration:
    migrations/00NN_your_change.sql

If you INTENTIONALLY re-baselined the migrations (rare, and only safe when no
database you care about has applied the old bytes), refreeze with:
    make freeze-migrations
and commit the updated migrations/.checksums.
MSG
  exit 1
fi

echo "OK: all frozen migrations are unchanged ($(grep -cvE '^\s*#|^\s*$' "$MANIFEST") files)."
