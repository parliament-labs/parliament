#!/usr/bin/env bash
# integration-test-sql harness — runs curated plane-representative SQL
# against seeded Pagila (Postgres) and Sakila (MySQL), compares against
# golden output, reports drift.
#
# Usage:
#   run.sh check     — diff current output against golden/ (default)
#   run.sh capture   — overwrite golden/ with current output (baseline)
#   run.sh list      — list configured queries
set -euo pipefail

SKILL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
QUERIES_DIR="$SKILL_DIR/queries"
GOLDEN_DIR="$SKILL_DIR/golden"
OUTPUT_DIR="$SKILL_DIR/.output"

CONN_FILE="${PARLIAMENT_CONNECTIONS:-$HOME/.sqlclient/connections.json}"

# Look up a field from the connections.json entry whose name matches.
# Silent on miss so env-var fallback stays clean.
conn_field() {
  local name="$1" field="$2"
  [[ -f "$CONN_FILE" ]] || return 0
  command -v jq >/dev/null 2>&1 || return 0
  jq -r --arg n "$name" --arg f "$field" \
    '.[] | select(.name == $n) | .[$f] // empty' "$CONN_FILE" 2>/dev/null
}

# Env var takes precedence; connections.json is the fallback; static default last.
PG_HOST="${PARLIAMENT_PG_HOST:-$(conn_field "Local Postgres" host)}"
PG_HOST="${PG_HOST:-127.0.0.1}"
PG_PORT="${PARLIAMENT_PG_PORT:-$(conn_field "Local Postgres" port)}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PARLIAMENT_PG_USER:-$(conn_field "Local Postgres" user)}"
PG_USER="${PG_USER:-postgres}"
PG_DB="${PARLIAMENT_PG_DB:-$(conn_field "Local Postgres" database)}"
PG_DB="${PG_DB:-pagila}"
PG_PASS="${PARLIAMENT_PG_PASS:-$(conn_field "Local Postgres" password)}"

MY_HOST="${PARLIAMENT_MY_HOST:-$(conn_field "Local MySQL" host)}"
MY_HOST="${MY_HOST:-127.0.0.1}"
MY_PORT="${PARLIAMENT_MY_PORT:-$(conn_field "Local MySQL" port)}"
MY_PORT="${MY_PORT:-3306}"
MY_USER="${PARLIAMENT_MY_USER:-$(conn_field "Local MySQL" user)}"
MY_USER="${MY_USER:-root}"
MY_DB="${PARLIAMENT_MY_DB:-$(conn_field "Local MySQL" database)}"
MY_DB="${MY_DB:-sakila}"
MY_PASS="${PARLIAMENT_MY_PASS:-$(conn_field "Local MySQL" password)}"

mode="${1:-check}"

run_pg() {
  if [[ -n "${PG_PASS:-}" ]]; then
    PGPASSWORD="$PG_PASS" psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$PG_DB" \
      --no-psqlrc --pset=footer=off --pset=border=2 -At -f "$1" 2>&1
  else
    psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$PG_DB" \
      --no-psqlrc --pset=footer=off --pset=border=2 -At -f "$1" 2>&1
  fi
}

run_my() {
  if [[ -n "${MY_PASS:-}" ]]; then
    MYSQL_PWD="$MY_PASS" mysql -h "$MY_HOST" -P "$MY_PORT" -u "$MY_USER" --batch --raw "$MY_DB" < "$1" 2>&1
  else
    mysql -h "$MY_HOST" -P "$MY_PORT" -u "$MY_USER" --batch --raw "$MY_DB" < "$1" 2>&1
  fi
}

list_queries() {
  find "$QUERIES_DIR" -name '*.sql' -type f 2>/dev/null | sort | while read -r q; do
    rel="${q#$QUERIES_DIR/}"
    echo "  $rel"
  done
}

if [[ "$mode" == "list" ]]; then
  echo "Queries:"
  list_queries
  exit 0
fi

if [[ ! -d "$QUERIES_DIR/postgres" ]] && [[ ! -d "$QUERIES_DIR/mysql" ]]; then
  echo "No queries directory found. Add .sql files under queries/postgres/ and queries/mysql/." >&2
  exit 1
fi

mkdir -p "$OUTPUT_DIR/postgres" "$OUTPUT_DIR/mysql" "$GOLDEN_DIR/postgres" "$GOLDEN_DIR/mysql"

drift=0
total=0

run_all() {
  local dbms="$1"
  local runner="$2"
  local dir="$QUERIES_DIR/$dbms"
  [[ -d "$dir" ]] || return 0
  for q in "$dir"/*.sql; do
    [[ -e "$q" ]] || continue
    local name
    name=$(basename "$q" .sql)
    local out="$OUTPUT_DIR/$dbms/$name.txt"
    local gold="$GOLDEN_DIR/$dbms/$name.txt"
    total=$((total + 1))
    if ! $runner "$q" > "$out" 2>&1; then
      echo "ERROR running $dbms/$name.sql — see $out" >&2
      drift=$((drift + 1))
      continue
    fi
    if [[ "$mode" == "capture" ]]; then
      cp "$out" "$gold"
      echo "captured $dbms/$name"
    else
      if [[ ! -f "$gold" ]]; then
        echo "MISSING GOLDEN: $dbms/$name (run './run.sh capture' to baseline)" >&2
        drift=$((drift + 1))
        continue
      fi
      if ! diff -u "$gold" "$out" > /dev/null 2>&1; then
        echo "DRIFT: $dbms/$name"
        diff -u "$gold" "$out" || true
        drift=$((drift + 1))
      fi
    fi
  done
}

run_all postgres run_pg
run_all mysql run_my

if [[ "$mode" == "capture" ]]; then
  echo ""
  echo "Captured baseline for $total queries. Review golden/ and commit if correct."
  exit 0
fi

echo ""
if [[ $drift -eq 0 ]]; then
  echo "No drift — $total queries match golden."
  exit 0
else
  echo "Drift detected in $drift of $total queries." >&2
  exit 1
fi
