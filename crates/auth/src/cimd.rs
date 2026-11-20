use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use reqwest::header::ACCEPT;
use reqwest::redirect::Policy;
use serde::Deserialize;
use url::Url;

const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_DOCUMENT: usize = 64 * 1024;
const MAX_NAME: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientApp {
    pub(crate) id: String,
    pub(crate) name: String,
    redirect_uris: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ClientError {
    #[error("client_id must be an https URL with a path, and no query or fragment")]
    InvalidId,
    #[error("client_id points to a private network")]
    PrivateAddress,
    #[error("the metadata document of the client could not be fetched: {0}")]
    Fetch(String),
    #[error("the metadata document of the client is invalid: {0}")]
    Invalid(String),
}

#[derive(Deserialize)]
struct Document {
    client_id: String,
    client_name: String,
    redirect_uris: Vec<String>,
    token_endpoint_auth_method: Option<String>,
}

pub(crate) async fn fetch(client_id: &str) -> Result<ClientApp, ClientError> {
    let url = id_url(client_id)?;
    let host = url.host_str().ok_or(ClientError::InvalidId)?.to_owned();
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|error| ClientError::Fetch(error.to_string()))?
        .collect();
    // every address must be public, or a rebinding resolver could pick the private one later
    if addresses.iter().any(|address| !public(address.ip())) {
        return Err(ClientError::PrivateAddress);
    }
    let address = addresses
        .first()
        .copied()
        .ok_or_else(|| ClientError::Fetch(format!("{host} has no address")))?;
    let fetch_error = |error: reqwest::Error| ClientError::Fetch(error.to_string());
    let client = reqwest::Client::builder()
        .resolve(&host, address)
        .redirect(Policy::none())
        .timeout(FETCH_TIMEOUT)
        .https_only(true)
        .no_proxy()
        .build()
        .map_err(fetch_error)?;
    let mut response = client
        .get(url)
        .header(ACCEPT, "application/json")
        .send()
        .await
        .map_err(fetch_error)?;
    if !response.status().is_success() {
        return Err(ClientError::Fetch(format!("status {}", response.status())));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(fetch_error)? {
        if body.len() + chunk.len() > MAX_DOCUMENT {
            return Err(ClientError::Fetch("the document is too big".to_owned()));
        }
        body.extend_from_slice(&chunk);
    }
    parse(&body, client_id)
}

pub(crate) fn parse(body: &[u8], client_id: &str) -> Result<ClientApp, ClientError> {
    let document: Document =
        serde_json::from_slice(body).map_err(|error| ClientError::Invalid(error.to_string()))?;
    if document.client_id != client_id {
        return Err(ClientError::Invalid(
            "its client_id is not the URL it was fetched from".to_owned(),
        ));
    }
    if document
        .token_endpoint_auth_method
        .as_deref()
        .is_some_and(|method| method != "none")
    {
        return Err(ClientError::Invalid(
            "only public clients are accepted: token_endpoint_auth_method must be none".to_owned(),
        ));
    }
    if document.redirect_uris.is_empty() {
        return Err(ClientError::Invalid("it lists no redirect_uris".to_owned()));
    }
    if let Some(unsafe_uri) = document
        .redirect_uris
        .iter()
        .find(|uri| !allowed_redirect(uri))
    {
        return Err(ClientError::Invalid(format!(
            "redirect URI {unsafe_uri:?} is neither https nor on this machine"
        )));
    }
    let name: String = document.client_name.trim().chars().take(MAX_NAME).collect();
    if name.is_empty() {
        return Err(ClientError::Invalid("its client_name is empty".to_owned()));
    }
    Ok(ClientApp {
        id: document.client_id,
        name,
        redirect_uris: document.redirect_uris,
    })
}

impl ClientApp {
    /// On loopback any port matches, since a native app listens on a free one (RFC 8252)
    pub(crate) fn redirects_to(&self, requested: &str) -> bool {
        self.redirect_uris
            .iter()
            .any(|registered| same_redirect(registered, requested))
    }
}

fn id_url(client_id: &str) -> Result<Url, ClientError> {
    let url = Url::parse(client_id).map_err(|_| ClientError::InvalidId)?;
    let valid = url.scheme() == "https"
        && url.host_str().is_some()
        && url.path() != "/"
        && url.query().is_none()
        && url.fragment().is_none()
        && url.username().is_empty();
    if valid {
        Ok(url)
    } else {
        Err(ClientError::InvalidId)
    }
}

fn allowed_redirect(uri: &str) -> bool {
    Url::parse(uri).is_ok_and(|url| {
        url.fragment().is_none()
            && (url.scheme() == "https" || (url.scheme() == "http" && loopback(&url)))
    })
}

fn same_redirect(registered: &str, requested: &str) -> bool {
    if registered == requested {
        return true;
    }
    let (Ok(registered), Ok(requested)) = (Url::parse(registered), Url::parse(requested)) else {
        return false;
    };
    registered.scheme() == "http"
        && requested.scheme() == "http"
        && loopback(&registered)
        && registered.host_str() == requested.host_str()
        && registered.path() == requested.path()
        && registered.query() == requested.query()
        && requested.fragment().is_none()
}

fn loopback(url: &Url) -> bool {
    matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
}

fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(mapped) => public_v4(mapped),
            None => public_v6(ip),
        },
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    let shared = a == 100 && (64..128).contains(&b);
    let benchmark = a == 198 && (b == 18 || b == 19);
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || shared
        || benchmark
        || a == 0
        || a >= 240)
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments().first().copied().unwrap_or_default();
    let unique_local = first & 0xfe00 == 0xfc00;
    let link_local = first & 0xffc0 == 0xfe80;
    let documentation = first == 0x2001 && ip.segments().get(1).copied() == Some(0x0db8);
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || unique_local
        || link_local
        || documentation)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE_CODE: &str = "https://claude.ai/oauth/claude-code-client-metadata";

    #[test]
    fn only_addresses_of_the_public_internet_are_fetched() {
        let refused = [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ];
        for address in refused {
            assert!(!public(address.parse().unwrap()), "fetched {address}");
        }
        for address in ["160.79.104.10", "2606:4700::1111"] {
            assert!(public(address.parse().unwrap()), "refused {address}");
        }
    }

    #[test]
    fn a_document_must_name_itself_and_list_safe_redirects() {
        let document = |client_id: &str, redirect: &str, method: &str| {
            serde_json::json!({
                "client_id": client_id,
                "client_name": "Claude Code",
                "redirect_uris": [redirect],
                "token_endpoint_auth_method": method,
            })
            .to_string()
        };
        let good = document(CLAUDE_CODE, "http://localhost/callback", "none");
        assert!(parse(good.as_bytes(), CLAUDE_CODE).is_ok());
        let refused = [
            document(
                "https://evil.example/doc",
                "http://localhost/callback",
                "none",
            ),
            document(CLAUDE_CODE, "http://evil.example/callback", "none"),
            document(
                CLAUDE_CODE,
                "http://localhost/callback",
                "client_secret_basic",
            ),
        ];
        for body in refused {
            assert!(
                parse(body.as_bytes(), CLAUDE_CODE).is_err(),
                "accepted {body}"
            );
        }
    }

    #[test]
    fn a_loopback_redirect_matches_on_any_port_and_others_exactly() {
        let document = serde_json::json!({
            "client_id": CLAUDE_CODE,
            "client_name": "Claude Code",
            "redirect_uris": ["http://localhost/callback", "https://claude.ai/api/mcp/auth_callback"],
        });
        let app = parse(document.to_string().as_bytes(), CLAUDE_CODE).unwrap();
        let cases = [
            ("http://localhost:3118/callback", true),
            ("http://localhost/callback", true),
            ("https://claude.ai/api/mcp/auth_callback", true),
            ("http://localhost:3118/other", false),
            ("http://127.0.0.1:3118/callback", false),
            ("https://claude.ai:8443/api/mcp/auth_callback", false),
            ("https://claude.ai/api/mcp/auth_callback?x=1", false),
        ];
        for (requested, expected) in cases {
            assert_eq!(app.redirects_to(requested), expected, "{requested}");
        }
    }
}
