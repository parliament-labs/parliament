# Project Parliament

Project Parliament is a modern database management client, consisting of two main components, Tyto and Noctua, named after distinct species of owls. The project symbolizes wisdom, insight, and cohesive collaboration in the world of database management.

## Components

### Project Tyto (Frontend) ✅ **Fully Functional**
The frontend web application providing an intuitive interface for database monitoring and management. Features live backend integration, real-time status updates, comprehensive database management capabilities, and an interactive database browser with tree navigation. Runs on port 3001 with responsive web UI and AJAX-powered dynamic content updates.

### Project Noctua (Backend) ✅ **Fully Functional**
The backend server providing secure database connectivity, encrypted credential storage, comprehensive PostgreSQL & MySQL management with complete SQL validation, and database introspection capabilities. Features HTTP/REST API (port 8080), gRPC service with TLS encryption, real-time monitoring, interactive database browsing endpoints, and multi-environment support for cloud and on-premises databases.

## Directory Structure

- /tyto: Frontend codebase (Project Tyto)
- /noctua: Backend codebase (Project Noctua)
- /shared: Shared resources, configurations, libraries, etc.
- /docs: Documentation, design specs, and additional project information.

## Getting Started

### Quick Start
1. **Prerequisites**: Rust 1.70+, Git
2. **Build**: `cargo build`
3. **Test**: `cargo test` 
4. **Run**: `cargo run --bin noctua_server`
5. **Access**: http://127.0.0.1:8080

### Documentation
- **📚 [Complete Documentation](docs/)** - Architecture, setup, design, and API reference
- **🔧 [Development Guide](CLAUDE.md)** - Commands and workflow for Claude Code
- **🎯 [Project Philosophy](VIBE_CODING.md)** - "Vibe coding with professional-grade discipline"
- **🦉 [Naming Convention](NAMING.md)** - Owl-themed component naming rationale

## Contributing
- Use feature branches: `feat/issue-X-description`
- All commits must pass: `cargo test` and `cargo clippy`
- See `/docs/setup/` for detailed development workflow
- Follow the vibe coding manifesto: professional discipline with creative spirit

For detailed information, see component READMEs in `/noctua/`, `/tyto/`, `/shared/`, and comprehensive documentation in `/docs/`.
