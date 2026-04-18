//! Response-wide HTTP security headers.
//!
//! Applied as a tower layer outside every other middleware so inner handlers
//! can still override any header by setting their own value first — the
//! defaults here only fill in what's missing.

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderName, HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;

pub async fn layer(req: Request, next: Next) -> Response {
    let is_https = request_is_https(&req);
    let mut response = next.run(req).await;
    apply_defaults(response.headers_mut(), is_https);
    response
}

fn apply_defaults(headers: &mut HeaderMap, is_https: bool) {
    // Always-on security posture. Static values, no per-request logic.
    set_default(headers, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    set_default(headers, header::X_FRAME_OPTIONS, "DENY");
    set_default(
        headers,
        header::REFERRER_POLICY,
        "strict-origin-when-cross-origin",
    );
    set_default(
        headers,
        HeaderName::from_static("cross-origin-opener-policy"),
        "same-origin",
    );
    set_default(
        headers,
        HeaderName::from_static("cross-origin-resource-policy"),
        "same-origin",
    );
    set_default(
        headers,
        HeaderName::from_static("permissions-policy"),
        PERMISSIONS_POLICY,
    );
    set_default(
        headers,
        HeaderName::from_static("x-download-options"),
        "noopen",
    );
    set_default(
        headers,
        HeaderName::from_static("x-permitted-cross-domain-policies"),
        "none",
    );
    // Legacy header; current OWASP guidance is to disable the reflected-XSS
    // filter (it has known bypasses and CSP is the proper replacement).
    set_default(headers, HeaderName::from_static("x-xss-protection"), "0");

    // Conservative cache defaults. Routes that deliberately want to cache
    // (hashed static assets, public GETs) set their own Cache-Control before
    // this middleware runs, which prevents the default from being applied.
    set_default(headers, header::CACHE_CONTROL, "no-store");
    set_default(headers, header::PRAGMA, "no-cache");
    set_default(headers, header::VARY, "Accept-Encoding");

    // HSTS is a no-op over plain HTTP per RFC 6797, but only emit it on
    // connections we can actually verify came in over TLS — direct HTTPS or
    // a reverse proxy that honored X-Forwarded-Proto. Prevents a misconfigured
    // proxy from accidentally shipping an HSTS header for a host that isn't
    // actually HTTPS-terminated.
    if is_https {
        set_default(
            headers,
            HeaderName::from_static("strict-transport-security"),
            "max-age=63072000; includeSubDomains",
        );
    }
}

const PERMISSIONS_POLICY: &str = "\
accelerometer=(), \
autoplay=(), \
camera=(), \
display-capture=(), \
encrypted-media=(), \
fullscreen=(), \
geolocation=(), \
gyroscope=(), \
magnetometer=(), \
microphone=(), \
midi=(), \
payment=(), \
picture-in-picture=(), \
publickey-credentials-get=(), \
screen-wake-lock=(), \
usb=(), \
web-share=(), \
xr-spatial-tracking=()\
";

fn set_default(headers: &mut HeaderMap, name: HeaderName, value: &'static str) {
    if !headers.contains_key(&name) {
        headers.insert(name, HeaderValue::from_static(value));
    }
}

fn request_is_https(req: &Request) -> bool {
    if let Some(proto) = req
        .headers()
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
    {
        // X-Forwarded-Proto may be a comma-separated list if the request went
        // through multiple proxies; the leftmost value reflects the origin.
        let first = proto.split(',').next().unwrap_or(proto).trim();
        if first.eq_ignore_ascii_case("https") {
            return true;
        }
    }
    req.uri().scheme_str() == Some("https")
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request as HttpRequest, Response};

    use super::*;

    fn req(uri: &str, extra_headers: &[(&str, &str)]) -> Request {
        let mut builder = HttpRequest::builder().uri(uri).method("GET");
        for (k, v) in extra_headers {
            builder = builder.header(*k, *v);
        }
        builder.body(Body::empty()).unwrap()
    }

    fn headers_after(req: &Request, seeded: &[(&str, &str)]) -> HeaderMap {
        let is_https = request_is_https(req);
        let mut response: Response<Body> = Response::new(Body::empty());
        for (k, v) in seeded {
            response.headers_mut().insert(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        apply_defaults(response.headers_mut(), is_https);
        response.into_parts().0.headers
    }

    #[test]
    fn core_headers_are_applied() {
        let headers = headers_after(&req("/", &[]), &[]);
        assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
        assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
        assert_eq!(
            headers.get("referrer-policy").unwrap(),
            "strict-origin-when-cross-origin"
        );
        assert_eq!(
            headers.get("cross-origin-opener-policy").unwrap(),
            "same-origin"
        );
        assert_eq!(
            headers.get("cross-origin-resource-policy").unwrap(),
            "same-origin"
        );
        assert!(headers.contains_key("permissions-policy"));
        assert_eq!(headers.get("x-download-options").unwrap(), "noopen");
        assert_eq!(
            headers.get("x-permitted-cross-domain-policies").unwrap(),
            "none"
        );
        assert_eq!(headers.get("x-xss-protection").unwrap(), "0");
        assert_eq!(headers.get("cache-control").unwrap(), "no-store");
        assert_eq!(headers.get("pragma").unwrap(), "no-cache");
        assert_eq!(headers.get("vary").unwrap(), "Accept-Encoding");
    }

    #[test]
    fn existing_cache_control_is_not_overridden() {
        // Static assets set their own cache-control with long immutability.
        // The middleware default must not clobber it.
        let headers = headers_after(&req("/assets/app-abc.js", &[]), &[(
            "cache-control",
            "public, max-age=31536000, immutable",
        )]);
        assert_eq!(
            headers.get("cache-control").unwrap(),
            "public, max-age=31536000, immutable"
        );
    }

    #[test]
    fn hsts_is_added_when_x_forwarded_proto_is_https() {
        let headers = headers_after(&req("/", &[("x-forwarded-proto", "https")]), &[]);
        assert_eq!(
            headers.get("strict-transport-security").unwrap(),
            "max-age=63072000; includeSubDomains"
        );
    }

    #[test]
    fn hsts_is_skipped_on_plain_http() {
        let headers = headers_after(&req("/", &[]), &[]);
        assert!(!headers.contains_key("strict-transport-security"));

        let headers = headers_after(&req("/", &[("x-forwarded-proto", "http")]), &[]);
        assert!(!headers.contains_key("strict-transport-security"));
    }

    #[test]
    fn hsts_reads_leftmost_value_of_chained_x_forwarded_proto() {
        // When a request flows through multiple proxies, the leftmost value
        // represents the original client → edge connection.
        let headers = headers_after(&req("/", &[("x-forwarded-proto", "https, http")]), &[]);
        assert!(headers.contains_key("strict-transport-security"));

        let headers = headers_after(&req("/", &[("x-forwarded-proto", "http, https")]), &[]);
        assert!(!headers.contains_key("strict-transport-security"));
    }
}
