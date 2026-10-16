# Noctua (Backend)

Noctua is the backend component of Project Parliament, named after the little owl genus associated with the Greek goddess Athena. It serves as the engine driving the project's database management functionality.

## Overview

Noctua is a modern Rust-based HTTP server that provides:
- RESTful API endpoints for database operations
- System health and introspection capabilities  
- Graceful shutdown and lifecycle management
- Real-time metrics and request tracking

## Architecture

- **Framework**: Axum with Tokio async runtime
- **State Management**: Thread-safe shared application state
- **Testing**: Comprehensive integration test suite
- **Endpoints**: Version, status, host info, and system control

## Quick Start

```bash
# Build and run the server
cargo run --bin noctua_server

# Run tests
cargo test

# Server will be available at http://127.0.0.1:8080
```

## Available Endpoints

- `GET /ping` - Health check
- `GET /version` - Server version information
- `GET /status` - Comprehensive server status
- `GET /hostinfo` - System information
- `POST /shutdown` - Graceful shutdown

## Development

See `/docs/setup/` for detailed development setup instructions and `/docs/commands/` for complete API reference.

The server follows the "vibe coding with professional-grade discipline" philosophy, emphasizing clean, modular code with comprehensive testing.
