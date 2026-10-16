//! The frontend built into the binary (`--features embed-frontend`), for
//! the release: Trunk's `dist/` — `trunk build --release` first — goes in
//! at compile time, so noctua serves Tyto from wherever it's run. Without
//! the feature noctua serves `dist/` from the directory it's started in,
//! and development goes through `trunk serve` as before.

use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

// `debug-embed` builds it in on a debug build too, so the folder is read
// relative to this crate, not to where noctua runs.
#[derive(rust_embed::RustEmbed)]
#[folder = "../dist/"]
struct Dist;

/// The file `uri` names — `/` being `index.html` — with its type, or 404.
pub(crate) async fn serve(uri: Uri) -> Response {
    let path = match uri.path().trim_start_matches('/') {
        "" => "index.html",
        path => path,
    };
    match Dist::get(path) {
        Some(file) => ([(header::CONTENT_TYPE, file.metadata.mimetype().to_owned())], file.data).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
