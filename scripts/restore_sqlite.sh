#!/bin/sh
# Replace a stopped gateway's SQLite database with a verified backup.
set -eu

if [ "$#" -ne 2 ]; then
    echo "usage: restore_sqlite.sh DATABASE_PATH BACKUP_PATH" >&2
    echo "stop the gateway before restoring; existing database is preserved as a dated copy" >&2
    exit 2
fi

database=$1
backup=$2
if [ ! -f "$backup" ]; then
    echo "backup does not exist: $backup" >&2
    exit 1
fi
if ! command -v sqlite3 >/dev/null 2>&1; then
    echo "sqlite3 CLI is required" >&2
    exit 1
fi

database_dir=$(dirname "$database")
mkdir -p "$database_dir"
timestamp=$(date -u '+%Y%m%dT%H%M%SZ')
temporary="$database.restore.$$"
previous="$database.pre-restore-$timestamp"
trap 'rm -f "$temporary"' EXIT HUP INT TERM
quoted_temporary=$(printf '%s' "$temporary" | sed 's/\\/\\\\/g; s/"/\\"/g')
sqlite3 "$backup" '.timeout 30000' ".backup \"$quoted_temporary\""
result=$(sqlite3 "$temporary" 'PRAGMA integrity_check;')
if [ "$result" != "ok" ]; then
    printf 'backup integrity check failed: %s\n' "$result" >&2
    exit 1
fi
chmod 600 "$temporary"
if [ -f "$database" ]; then
    quoted_previous=$(printf '%s' "$previous" | sed 's/\\/\\\\/g; s/"/\\"/g')
    # Use SQLite's backup API for the safety copy too, so any committed WAL pages are included.
    sqlite3 "$database" '.timeout 30000' ".backup \"$quoted_previous\""
    result=$(sqlite3 "$previous" 'PRAGMA integrity_check;')
    if [ "$result" != "ok" ]; then
        printf 'pre-restore database integrity check failed: %s\n' "$result" >&2
        exit 1
    fi
    chmod 600 "$previous"
fi
# The gateway must be stopped so no process can still have the old SQLite/WAL open.
# Remove sidecar files only after preserving the main database and before installing the copy.
rm -f "$database-wal" "$database-shm"
mv "$temporary" "$database"
trap - EXIT HUP INT TERM
printf 'restored %s\nprevious database: %s\n' "$database" "$previous"
