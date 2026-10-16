#!/bin/bash
#
# Download and install sample databases for development and screenshots.
#
# Pagila (PostgreSQL) — DVD rental store. Community-standard demo database.
# Sakila (MySQL)      — Same schema, MySQL-native.
#
# Usage:
#   ./tests/fixtures/setup.sh postgres    # Install Pagila into local Postgres
#   ./tests/fixtures/setup.sh mysql       # Install Sakila into local MySQL
#   ./tests/fixtures/setup.sh all         # Both
#
# Prerequisites:
#   - psql and/or mysql CLI tools installed
#   - Local databases running and accessible
#
# The script creates a dedicated database for each (pagila / sakila),
# drops it first if it exists, and loads fresh.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PAGILA_DIR="$SCRIPT_DIR/pagila"
SAKILA_DIR="$SCRIPT_DIR/sakila"

setup_pagila() {
    echo "=== Setting up Pagila (PostgreSQL) ==="

    # Download if not cached
    if [ ! -f "$PAGILA_DIR/pagila-schema.sql" ]; then
        echo "Downloading Pagila..."
        mkdir -p "$PAGILA_DIR"
        curl -sL "https://raw.githubusercontent.com/devrimgunduz/pagila/master/pagila-schema.sql" \
            -o "$PAGILA_DIR/pagila-schema.sql"
        curl -sL "https://raw.githubusercontent.com/devrimgunduz/pagila/master/pagila-data.sql" \
            -o "$PAGILA_DIR/pagila-data.sql"
        echo "Downloaded."
    else
        echo "Using cached Pagila files."
    fi

    # Create database
    echo "Creating database 'pagila'..."
    psql -h 127.0.0.1 -U postgres -c "DROP DATABASE IF EXISTS pagila;" 2>/dev/null || true
    psql -h 127.0.0.1 -U postgres -c "CREATE DATABASE pagila;"

    # Load schema + data
    echo "Loading schema..."
    psql -h 127.0.0.1 -U postgres -d pagila -f "$PAGILA_DIR/pagila-schema.sql" -q
    echo "Loading data..."
    psql -h 127.0.0.1 -U postgres -d pagila -f "$PAGILA_DIR/pagila-data.sql" -q

    echo "Pagila ready. Connect with: database = pagila"
    echo ""
}

setup_sakila() {
    echo "=== Setting up Sakila (MySQL) ==="

    # Download if not cached
    if [ ! -f "$SAKILA_DIR/sakila-schema.sql" ]; then
        echo "Downloading Sakila..."
        mkdir -p "$SAKILA_DIR"
        curl -sL "https://raw.githubusercontent.com/jOOQ/sakila/main/mysql-sakila-db/mysql-sakila-schema.sql" \
            -o "$SAKILA_DIR/sakila-schema.sql"
        curl -sL "https://raw.githubusercontent.com/jOOQ/sakila/main/mysql-sakila-db/mysql-sakila-insert-data.sql" \
            -o "$SAKILA_DIR/sakila-data.sql"
        echo "Downloaded."
    else
        echo "Using cached Sakila files."
    fi

    # Create database
    echo "Creating database 'sakila'..."
    mysql -h 127.0.0.1 -u root -e "DROP DATABASE IF EXISTS sakila; CREATE DATABASE sakila;" 2>/dev/null

    # Load schema + data
    echo "Loading schema..."
    mysql -h 127.0.0.1 -u root sakila < "$SAKILA_DIR/sakila-schema.sql"
    echo "Loading data..."
    mysql -h 127.0.0.1 -u root sakila < "$SAKILA_DIR/sakila-data.sql"

    echo "Sakila ready. Connect with: database = sakila"
    echo ""
}

case "${1:-all}" in
    postgres|pg)
        setup_pagila
        ;;
    mysql|my)
        setup_sakila
        ;;
    all)
        setup_pagila
        setup_sakila
        ;;
    *)
        echo "Usage: $0 {postgres|mysql|all}"
        exit 1
        ;;
esac

echo "Done. Sample databases are ready for development and screenshots."
