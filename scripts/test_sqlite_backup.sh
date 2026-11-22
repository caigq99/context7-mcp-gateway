#!/bin/sh
# Exercise online backup, verified restore, preservation, and corrupt-input fail-closed behavior.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/context7-sqlite-test.XXXXXX")
trap 'rm -rf "$temporary_dir"' EXIT HUP INT TERM

database="$temporary_dir/live database.sqlite3"
backup="$temporary_dir/offsite's backup.sqlite3"
corrupt="$temporary_dir/corrupt.sqlite3"
sqlite3 "$database" 'CREATE TABLE probe(value TEXT NOT NULL); INSERT INTO probe VALUES ("snapshot");'
"$script_dir/backup_sqlite.sh" "$database" "$backup" >/dev/null
[ "$(sqlite3 "$backup" 'PRAGMA integrity_check;')" = "ok" ]
sqlite3 "$database" 'INSERT INTO probe VALUES ("after-backup");'
"$script_dir/restore_sqlite.sh" "$database" "$backup" >/dev/null
[ "$(sqlite3 "$database" 'SELECT group_concat(value, ",") FROM probe;')" = "snapshot" ]
[ "$(sqlite3 "$database" 'PRAGMA integrity_check;')" = "ok" ]
[ "$(sqlite3 "$database" -batch 'SELECT count(*) FROM probe;' 2>/dev/null)" = "1" ]

printf 'not a SQLite database\n' > "$corrupt"
if "$script_dir/restore_sqlite.sh" "$database" "$corrupt" >/dev/null 2>&1; then
    echo "corrupt restore input was incorrectly accepted" >&2
    exit 1
fi
[ "$(sqlite3 "$database" 'SELECT group_concat(value, ",") FROM probe;')" = "snapshot" ]
printf '%s\n' 'SQLite backup/restore exercise passed.'
