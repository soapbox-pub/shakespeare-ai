//! Serving the built dashboard SPA (`dashboard/`).
//!
//! Unknown paths fall back to `index.html` so client-side routes work on a
//! hard refresh. Vite's hashed assets are immutable; everything else is
//! revalidated each time.
//!
//! With the `embedded-dashboard` feature the files are compiled in (see
//! build.rs), so a release is one binary whose dashboard can't drift from its
//! API. Without it they're read from disk, so the dashboard can be rebuilt
//! while the server runs.

use std::path::{Component, Path, PathBuf};

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

#[cfg(feature = "embedded-dashboard")]
mod embedded {
    include!(concat!(env!("OUT_DIR"), "/dashboard_files.rs"));
}

pub enum Source {
    /// The first of these directories with an `index.html`, looked for on
    /// each request, so building the dashboard needs no restart.
    #[cfg_attr(feature = "embedded-dashboard", allow(dead_code))]
    Disk(Vec<PathBuf>),
    #[cfg(feature = "embedded-dashboard")]
    Embedded,
}

/// Where this build's dashboard comes from: compiled in, or else
/// `dashboard/dist` under the working directory, beside the binary, or in the
/// source tree it was built from.
pub fn source() -> Source {
    #[cfg(feature = "embedded-dashboard")]
    return Source::Embedded;

    #[cfg(not(feature = "embedded-dashboard"))]
    {
        let mut candidates = vec![std::path::absolute("dashboard/dist").unwrap_or_else(|_| PathBuf::from("dashboard/dist"))];
        if let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
            candidates.push(dir.join("dashboard/dist"));
        }
        candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("dashboard/dist"));
        Source::Disk(candidates)
    }
}

impl Source {
    /// The file at a relative path, if the dashboard has one.
    fn read(&self, relative: &Path) -> Option<Vec<u8>> {
        match self {
            Source::Disk(candidates) => {
                let root = candidates.iter().find(|dir| dir.join("index.html").is_file())?;
                let path = root.join(relative);
                path.is_file().then(|| std::fs::read(path).ok()).flatten()
            }
            #[cfg(feature = "embedded-dashboard")]
            Source::Embedded => {
                let name = relative.to_str()?;
                embedded::FILES.iter().find(|(path, _)| *path == name).map(|(_, bytes)| bytes.to_vec())
            }
        }
    }

    /// The response for a GET or HEAD of `path`. Reads files, so call it on
    /// the blocking pool.
    pub fn serve(&self, path: &str, headers: &HeaderMap) -> Response {
        let relative = normalize(path);
        let (relative, bytes) = match self.read(&relative) {
            Some(bytes) => (relative, bytes),
            None => match self.read(Path::new("index.html")) {
                Some(bytes) => (PathBuf::from("index.html"), bytes),
                None => {
                    return (StatusCode::SERVICE_UNAVAILABLE, "The dashboard hasn't been built. Run `npm ci && npm run build` in dashboard/.")
                        .into_response();
                }
            },
        };

        let etag = format!("\"{}\"", &hex::encode(Sha256::digest(&bytes))[..32]);
        let immutable = relative.starts_with("assets");
        let mut response = if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()).is_some_and(|tags| tags.split(',').any(|t| t.trim() == etag)) {
            StatusCode::NOT_MODIFIED.into_response()
        } else {
            let mut response = Body::from(bytes).into_response();
            response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type(&relative)));
            response
        };
        let cache = if immutable { "public, max-age=31536000, immutable" } else { "no-cache" };
        response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
        if let Ok(etag) = HeaderValue::from_str(&etag) {
            response.headers_mut().insert(header::ETAG, etag);
        }
        response
    }
}

/// A URL path as a path relative to the dashboard's root. `..` can't escape
/// it: the path is normalized before anything joins it.
fn normalize(path: &str) -> PathBuf {
    let decoded = percent_encoding::percent_decode_str(path).decode_utf8_lossy();
    let mut relative = PathBuf::new();
    for component in Path::new(decoded.as_ref()).components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::ParentDir => {
                relative.pop();
            }
            _ => {}
        }
    }
    relative
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "webmanifest" => "application/manifest+json",
        "txt" => "text/plain; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_files_and_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("assets")).unwrap();
        std::fs::write(dir.path().join("index.html"), "<p>app</p>").unwrap();
        std::fs::write(dir.path().join("assets/app-1.js"), "js").unwrap();
        let source = Source::Disk(vec![dir.path().join("missing"), dir.path().to_path_buf()]);
        let none = HeaderMap::new();

        let response = source.serve("/assets/app-1.js", &none);
        assert_eq!(response.headers()["cache-control"], "public, max-age=31536000, immutable");
        assert_eq!(response.headers()["content-type"], "text/javascript; charset=utf-8");

        let response = source.serve("/credits", &none);
        assert_eq!(response.headers()["content-type"], "text/html; charset=utf-8");
        assert_eq!(response.headers()["cache-control"], "no-cache");

        for escape in ["/../../etc/passwd", "/%2e%2e/%2e%2e/etc/passwd"] {
            assert_eq!(source.serve(escape, &none).headers()["content-type"], "text/html; charset=utf-8");
        }

        let mut revalidate = HeaderMap::new();
        revalidate.insert(header::IF_NONE_MATCH, response.headers()["etag"].clone());
        assert_eq!(source.serve("/", &revalidate).status(), StatusCode::NOT_MODIFIED);
    }

    #[test]
    fn says_when_unbuilt() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Source::Disk(vec![dir.path().to_path_buf()]).serve("/", &HeaderMap::new()).status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
