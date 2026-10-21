//! The DeskWatch icon in the browser tab and on a phone home screen, served
//! by the bridge itself like the fonts: the status ring on the Graphite
//! background (`bridge/icons/`).
//!
//! - `GET /favicon.svg`: the icon itself, sharp at every size
//! - `GET /favicon-32.png`: a fallback for browsers without SVG icons
//! - `GET /apple-touch-icon.png`: 180x180, square, for "Add to Home Screen"
//!
//! The routes need no login: the login page shows the icon too, and an icon
//! says nothing about this install.

use axum::{
    Router,
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
    routing::get,
};

pub const FAVICON_SVG: &[u8] = include_bytes!("../icons/favicon.svg");
pub const FAVICON_PNG: &[u8] = include_bytes!("../icons/favicon-32.png");
pub const TOUCH_ICON: &[u8] = include_bytes!("../icons/apple-touch-icon.png");

pub fn router() -> Router {
    Router::new()
        .route("/favicon.svg", get(|| icon("image/svg+xml", FAVICON_SVG)))
        .route("/favicon-32.png", get(|| icon("image/png", FAVICON_PNG)))
        .route(
            "/apple-touch-icon.png",
            get(|| icon("image/png", TOUCH_ICON)),
        )
}

async fn icon(content_type: &'static str, body: &'static [u8]) -> Response {
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // An icon only changes with a new bridge; a day is plenty.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn serves_each_icon_with_its_type() {
        for (path, content_type, magic) in [
            ("/favicon.svg", "image/svg+xml", &b"<svg"[..]),
            ("/favicon-32.png", "image/png", &b"\x89PNG"[..]),
            ("/apple-touch-icon.png", "image/png", &b"\x89PNG"[..]),
        ] {
            let response = router()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                content_type,
                "{path}"
            );
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            assert_eq!(&body[..magic.len()], magic, "{path}");
        }
    }
}
