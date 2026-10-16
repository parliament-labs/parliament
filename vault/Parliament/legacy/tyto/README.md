# Tyto - Parliament Frontend

Tyto is Parliament's fully functional web frontend, providing an intuitive interface for database monitoring and management. Named after the Tyto genus of barn owls, it serves as the user-facing component that connects to the Noctua backend.

## Overview

Tyto is a modern web application built with Rust and Axum that provides:

- **Live Backend Integration**: Real-time communication with Noctua backend
- **Dynamic Status Updates**: AJAX-powered real-time status monitoring  
- **Database Management Interface**: Comprehensive database registration and testing
- **Interactive Database Browser**: Tree navigation, table structure viewing, and data browsing
- **Responsive Design**: Modern, mobile-friendly user interface
- **API Explorer**: Interactive access to all backend endpoints

## Architecture

### Technology Stack

- **Framework**: Axum (async web framework)
- **Templating**: Askama (compile-time templates)
- **Styling**: Modern CSS with gradient design system
- **Interactivity**: Vanilla JavaScript for database browser and AJAX calls
- **HTTP Client**: reqwest for backend communication
- **Logging**: tracing with structured output

### Core Components

```
tyto/
├── src/
│   ├── main.rs           # Application entry point
│   ├── handlers/         # Request handlers
│   │   ├── home.rs       # Landing page and status
│   │   ├── health.rs     # Health checks
│   │   └── mod.rs        # Handler module exports
├── templates/            # Askama HTML templates
│   ├── landing.html      # Main application interface
│   └── test.html         # Testing interface
└── static/               # Static assets (CSS, JS)
    ├── css/
    └── js/
```

## Features

### 🏠 **Landing Page Dashboard**
- Real-time backend connection status
- Server uptime and request metrics
- Interactive API endpoint explorer
- Database management interface

### 🔌 **Live Backend Integration**
- Automatic connection to Noctua on port 8080
- Real-time status polling every 30 seconds
- Graceful handling of backend unavailability
- Dynamic status indicators and error messages

### 📊 **Server Monitoring**
- **Connection Status**: Visual indicators for backend availability
- **Uptime Tracking**: Human-readable uptime display
- **Request Metrics**: Total requests and rate monitoring
- **Auto-refresh**: HTMX-powered live updates without page reload

### 🗄️ **Database Management**
- **Registration Interface**: Add new PostgreSQL databases
- **Connection Testing**: Test database connectivity and permissions
- **Database Listing**: View all registered databases
- **Permission Validation**: Comprehensive monitoring permissions check

### 🎨 **User Interface**
- **Modern Design**: Professional gradient theme with owl branding
- **Responsive Layout**: Works on desktop, tablet, and mobile
- **Interactive Cards**: Hover effects and smooth transitions
- **Status Indicators**: Color-coded success/error states
- **Loading States**: Smooth HTMX transitions

## Running Tyto

### Quick Start

```bash
# From the tyto directory
cargo run

# Or from workspace root
cargo run --bin tyto
```

Tyto starts on http://127.0.0.1:3001 by default.

### Configuration

Tyto connects to Noctua backend at `http://127.0.0.1:8080`. Ensure Noctua is running:

```bash
# Start Noctua backend first
cargo run --bin noctua_server

# Then start Tyto frontend  
cargo run --bin tyto
```

### Environment Variables

```bash
# Logging level (optional)
RUST_LOG=tyto=debug,tower_http=debug

# Backend URL (if different from default)
NOCTUA_BACKEND_URL=http://127.0.0.1:8080
```

## API Integration

### Backend Communication

Tyto communicates with Noctua through HTTP APIs:

#### Status Monitoring
```rust
// Fetch real-time status from Noctua
let response = reqwest::get("http://127.0.0.1:8080/status").await;
```

#### Database Operations
- **Registration**: POST to `/database/register`
- **Testing**: POST to `/database/test-permissions`
- **Listing**: GET from `/database/list`
- **Health Checks**: GET from `/database/{id}/test`

### HTMX Integration

Tyto uses HTMX for dynamic content updates:

```html
<!-- Auto-refreshing status bar -->
<div class="status-bar" 
     hx-get="/api/status" 
     hx-trigger="load, every 30s" 
     hx-swap="innerHTML">
</div>

<!-- Interactive API examples -->
<button class="api-link" 
        hx-get="/examples/register" 
        hx-target="#example-modal" 
        hx-trigger="click">
    Show Example
</button>
```

## Development

### Local Development

```bash
# Install dependencies and run
cargo build
cargo run

# With debug logging
RUST_LOG=debug cargo run

# Watch for changes (if using cargo-watch)
cargo watch -x run
```

### Adding New Features

1. **Create Handler**: Add new handler in `src/handlers/`
2. **Add Route**: Register route in `src/main.rs`
3. **Create Template**: Add Askama template in `templates/`
4. **Add Styles**: Extend CSS in template or static files

### Template Development

Tyto uses Askama for compile-time templating:

```rust
#[derive(Template)]
#[template(path = "landing.html")]
struct LandingPageTemplate {
    server_status: ServerStatus,
}
```

Templates support:
- Variable interpolation: `{{ variable }}`
- Control flow: `{% if condition %}...{% endif %}`
- Loops: `{% for item in items %}...{% endfor %}`
- Inheritance and includes

## Testing

### Manual Testing

1. **Start Backend**: `cargo run --bin noctua_server`
2. **Start Frontend**: `cargo run --bin tyto`
3. **Visit Interface**: http://127.0.0.1:3001
4. **Test Features**:
   - Check backend connection status
   - Try API endpoint links
   - Monitor real-time status updates

### Integration Testing

```bash
# Run with backend available
cargo test

# Test with backend unavailable (simulate offline)
# Stop noctua_server and verify graceful degradation
```

## Production Deployment

### Build for Production

```bash
# Optimized build
cargo build --release

# Run production binary
./target/release/tyto
```

### Reverse Proxy Configuration

Example Nginx configuration:

```nginx
server {
    listen 80;
    server_name parliament.example.com;
    
    # Frontend
    location / {
        proxy_pass http://127.0.0.1:3001;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
    }
    
    # Backend API
    location /api/ {
        proxy_pass http://127.0.0.1:8080/;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
    }
}
```

## Error Handling

### Backend Unavailable

When Noctua backend is offline, Tyto:
- Displays clear error messages
- Shows "Offline" status indicators
- Continues serving the interface
- Automatically reconnects when backend returns

### Connection Failures

```rust
async fn fetch_noctua_status() -> ServerStatus {
    match reqwest::get("http://127.0.0.1:8080/status").await {
        Ok(response) => { /* Handle success */ }
        Err(err) => {
            tracing::warn!("Failed to connect to Noctua backend: {}", err);
            ServerStatus::default() // Graceful degradation
        }
    }
}
```

## Customization

### Styling

Tyto uses embedded CSS with CSS custom properties for easy theming:

```css
:root {
    --primary-color: #2a5298;
    --secondary-color: #1e3c72;
    --success-color: #28a745;
    --error-color: #dc3545;
}
```

### Adding New API Endpoints

1. **Add Route**: Register in main.rs router
2. **Create Handler**: Implement async handler function
3. **Update Template**: Add UI elements if needed
4. **Test Integration**: Verify backend communication

## Logging

Tyto provides structured logging for debugging and monitoring:

```rust
tracing::info!("🦉 Starting Tyto - Parliament Frontend");
tracing::warn!("Failed to connect to Noctua backend: {}", err);
```

### Log Levels

- **INFO**: Startup, requests, major events
- **WARN**: Backend connection issues, parse errors
- **DEBUG**: Detailed request/response information
- **ERROR**: Critical failures

## Future Enhancements

Tyto is ready for additional features:

1. **Database Query Interface**: SQL editor and result display
2. **Real-time Dashboards**: Live metrics and charts
3. **User Authentication**: Login and role-based access
4. **Database Schema Browser**: Visual schema exploration
5. **Query History**: Saved queries and execution history
6. **Alerting Interface**: Configure monitoring alerts
7. **Multi-backend Support**: Connect to multiple Noctua instances

## Contributing

When contributing to Tyto:

1. **Follow Rust conventions**: Use rustfmt and clippy
2. **Test thoroughly**: Verify both online and offline backend states
3. **Update templates**: Keep Askama templates in sync with handlers
4. **Document changes**: Update this README for new features
5. **Consider accessibility**: Ensure UI works for all users

Tyto represents the user-facing aspect of Parliament's database monitoring capabilities, providing an intuitive interface for the powerful backend functionality provided by Noctua.