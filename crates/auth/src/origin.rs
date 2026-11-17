use axum::extract::{OriginalUri, Request};
use axum::http::{HeaderMap, Uri, header};
use axum::middleware::Next;
use axum::response::Response;
use resysto_http::{AppError, AppResult};

/// Refuses a request that changes data when the browser says another site started it
pub async fn same_origin(request: Request, next: Next) -> AppResult<Response> {
    if !request.method().is_safe() && !from_this_origin(request.headers(), request.uri()) {
        let uri = request
            .extensions()
            .get::<OriginalUri>()
            .map_or(request.uri(), |original| &original.0);
        tracing::warn!(method = %request.method(), path = uri.path(), "cross-origin request refused");
        return Err(AppError::Forbidden);
    }
    Ok(next.run(request).await)
}

// Sec-Fetch-Site when the browser sends it, otherwise Origin against Host
fn from_this_origin(headers: &HeaderMap, uri: &Uri) -> bool {
    if let Some(site) = headers.get("sec-fetch-site") {
        return site == "same-origin" || site == "none";
    }
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
        .or_else(|| uri.authority().map(|authority| authority.as_str()));
    let authority = origin
        .to_str()
        .ok()
        .and_then(|origin| origin.split_once("://"))
        .map(|(_, authority)| authority);
    matches!((authority, host), (Some(authority), Some(host)) if authority == host)
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    #[test]
    fn a_write_passes_only_from_this_origin_or_from_a_client_that_is_not_a_browser() {
        let cases: [(&[(&str, &str)], bool); 8] = [
            (&[("sec-fetch-site", "same-origin")], true),
            (&[("sec-fetch-site", "none")], true),
            (&[("sec-fetch-site", "same-site")], false),
            (&[("sec-fetch-site", "cross-site")], false),
            (
                &[("origin", "https://app.acme.it"), ("host", "app.acme.it")],
                true,
            ),
            (
                &[("origin", "https://evil.acme.it"), ("host", "app.acme.it")],
                false,
            ),
            (&[("origin", "null"), ("host", "app.acme.it")], false),
            (&[("host", "app.acme.it")], true),
        ];
        for (sent, allowed) in cases {
            let mut headers = HeaderMap::new();
            for (name, value) in sent {
                headers.insert(*name, HeaderValue::from_static(value));
            }
            let got = from_this_origin(&headers, &Uri::from_static("/api/v1/auth/login"));
            assert_eq!(got, allowed, "{sent:?}");
        }
    }
}
