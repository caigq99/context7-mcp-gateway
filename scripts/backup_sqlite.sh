#!/bin/sh
# Make a transaction-consistent, online SQLite backup using sqlite3's backup API.
set -eu

if [ "$#" -ne 2 ]; then
    echo "usage: backup_sqlite.sh DATABASE_PATH BACKUP_PATH" >&2
    exit 2
fi

source_db=$1
destination=$2
if [ ! -f "$source_db" ]; then
    echo "database does not exist: $source_db" >&2
    exit 1
fi
if [ -e "$destination" ]; then
    echo "backup destination already exists: $destination" >&2
    exit 1
fi
if ! command -v sqlite3 >/dev/null 2>&1; then
    echo "sqlite3 CLI is required" >&2
    exit 1
fi

destination_dir=$(dirname "$destination")
mkdir -p "$destination_dir"
# Use a sibling temp file to keep the final rename on the same filesystem.
temporary="$destination.tmp.$$"
trap 'rm -f "$temporary"' EXIT HUP INT TERM
quoted_temporary=$(printf '%s' "$temporary" | sed 's/\\/\\\\/g; s/"/\\"/g')
sqlite3 "$source_db" '.timeout 30000' ".backup \"$quoted_temporary\""
result=$(sqlite3 "$temporary" 'PRAGMA integrity_check;')
if [ "$result" != "ok" ]; then
    printf 'backup integrity check failed: %s\n' "$result" >&2
    exit 1
fi
chmod 600 "$temporary"
mv "$temporary" "$destination"
trap - EXIT HUP INT TERM
printf '%s\n' "$destination"
