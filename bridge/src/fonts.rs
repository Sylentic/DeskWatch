//! The typefaces of the kiosk, setup and login pages, served by the bridge
//! itself so the pages need no internet: Geist, Geist Mono and Space Grotesk
//! (latin, variable weight, woff2). All three are under the SIL Open Font
//! License 1.1, see `bridge/fonts/`.
//!
//! - `GET /fonts/geist.woff2`, `/fonts/geist-mono.woff2`, `/fonts/space-grotesk.woff2`
//!
//! The routes need no login: the login page uses them too, and a font says
//! nothing about this install.

use axum::{
    Router,
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
    routing::get,
};

pub const GEIST: &[u8] = include_bytes!("../fonts/geist.woff2");
pub const GEIST_MONO: &[u8] = include_bytes!("../fonts/geist-mono.woff2");
pub const SPACE_GROTESK: &[u8] = include_bytes!("../fonts/space-grotesk.woff2");

pub fn router() -> Router {
    Router::new()
        .route("/fonts/geist.woff2", get(|| font(GEIST)))
        .route("/fonts/geist-mono.woff2", get(|| font(GEIST_MONO)))
        .route("/fonts/space-grotesk.woff2", get(|| font(SPACE_GROTESK)))
}

async fn font(body: &'static [u8]) -> Response {
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("font/woff2"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // A font only changes with a new bridge; a day is plenty.
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
    async fn serves_each_font_as_woff2() {
        for path in [
            "/fonts/geist.woff2",
            "/fonts/geist-mono.woff2",
            "/fonts/space-grotesk.woff2",
        ] {
            let response = router()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "font/woff2",
                "{path}"
            );
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            assert_eq!(&body[..4], b"wOF2", "{path}");
        }
    }
}
