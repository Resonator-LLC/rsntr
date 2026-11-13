//! The embedded web client: `client/dist`, staged by `build.rs`.

use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use resonator_server::http::AppState;
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "$OUT_DIR/client"]
struct Client;

/// Vite gives everything under `assets/` a content hash in its name.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
/// The page itself must be revalidated so a new build is picked up.
const REVALIDATE: &str = "no-cache";

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn file(path: &str, cache: &'static str) -> Response {
    match Client::get(path) {
        Some(file) => (
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static(content_type(path)),
                ),
                (header::CACHE_CONTROL, HeaderValue::from_static(cache)),
            ],
            file.data,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn index() -> Response {
    file("index.html", REVALIDATE)
}

async fn asset(Path(path): Path<String>) -> Response {
    file(&format!("assets/{path}"), IMMUTABLE)
}

async fn favicon() -> Response {
    file("favicon.svg", REVALIDATE)
}

/// The web app manifest, for installing the client as an app.
async fn manifest() -> Response {
    file("manifest.webmanifest", REVALIDATE)
}

/// The client build this server serves: the file name of the script
/// `index.html` loads (`index-<hash>.js`), which changes with every build.
fn client_build() -> &'static str {
    static BUILD: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BUILD.get_or_init(|| {
        Client::get("index.html")
            .and_then(|file| build_of(&String::from_utf8_lossy(&file.data)))
            .unwrap_or_default()
    })
}

/// The name of the first `/assets/….js` script in a page.
fn build_of(html: &str) -> Option<String> {
    html.split("/assets/").skip(1).find_map(|rest| {
        let name = &rest[..rest.find(['"', '\''])?];
        (name.ends_with(".js") && !name.contains(['/', ' '])).then(|| name.to_owned())
    })
}

/// `{"client": "<build>"}`: an open page compares it with the build it
/// runs and offers to reload when they differ.
async fn version() -> Response {
    // `build_of` admits no quote, slash or space, and Vite's names are
    // `[A-Za-z0-9_.-]`: nothing to escape.
    (
        [
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            ),
        ],
        format!(r#"{{"client":"{}"}}"#, client_build().replace('\\', "")),
    )
        .into_response()
}

/// The app icons of the manifest and of iOS's home screen.
const ICONS: [&str; 3] = ["icon-180.png", "icon-192.png", "icon-512.png"];

/// `/`, `/assets/*`, `/favicon.svg`, `/manifest.webmanifest` and the
/// `/icon-<size>.png` app icons.
pub fn routes() -> Router<AppState> {
    let mut router = Router::new()
        .route("/", get(index))
        .route("/assets/{*path}", get(asset))
        .route("/favicon.svg", get(favicon))
        .route("/manifest.webmanifest", get(manifest))
        .route("/version", get(version));
    for icon in ICONS {
        router = router.route(
            &format!("/{icon}"),
            get(move || async move { file(icon, REVALIDATE) }),
        );
    }
    router
}

#[cfg(test)]
mod tests {
    use super::build_of;

    #[test]
    fn the_build_is_the_name_of_the_page_script() {
        let page = r#"<link rel="stylesheet" href="/assets/index-Ab1.css">
<script type="module" crossorigin src="/assets/index-B3h3E6m_.js"></script>"#;
        assert_eq!(build_of(page).as_deref(), Some("index-B3h3E6m_.js"));
        assert_eq!(build_of("<p>no script</p>"), None);
    }
}
