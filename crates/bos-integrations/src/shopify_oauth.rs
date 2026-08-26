//! Shopify Admin API token exchange helpers. Config-driven: callers pass app
//! credentials explicitly; this module never reads env vars.

use serde_json::Value;

#[derive(Clone)]
pub struct ShopifyOAuthApp {
    pub shop_domain: String,
    pub client_id: String,
    pub client_secret: String,
    /// Token endpoint override (tests). `None` = the shop's Admin OAuth endpoint.
    pub token_url: Option<String>,
}

impl std::fmt::Debug for ShopifyOAuthApp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShopifyOAuthApp")
            .field("shop_domain", &self.shop_domain)
            .field("client_id", &self.client_id)
            .field("client_secret", &"[redacted]")
            .field("token_url", &self.token_url)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShopifyTokenError {
    RateLimited { message: String },
    Rejected { message: String },
    Retryable { code: String, message: String },
    Permanent { code: String, message: String },
}

impl std::fmt::Display for ShopifyTokenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateLimited { message } => write!(formatter, "rate_limited: {message}"),
            Self::Rejected { message } => write!(formatter, "rejected: {message}"),
            Self::Retryable { code, message } => write!(formatter, "{code}: {message}"),
            Self::Permanent { code, message } => write!(formatter, "{code}: {message}"),
        }
    }
}

pub fn normalize_shop_domain(raw: &str) -> String {
    raw.trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string()
}

pub fn fetch_access_token(app: &ShopifyOAuthApp) -> Result<String, ShopifyTokenError> {
    let shop_domain = normalize_shop_domain(&app.shop_domain);
    if shop_domain.is_empty() {
        return Err(ShopifyTokenError::Permanent {
            code: "shopify_shop_domain_missing".to_string(),
            message: "Shopify shop domain is required for token exchange".to_string(),
        });
    }
    if app.client_id.trim().is_empty() || app.client_secret.trim().is_empty() {
        return Err(ShopifyTokenError::Permanent {
            code: "shopify_client_credentials_missing".to_string(),
            message: "Shopify client id and client secret are required for token exchange"
                .to_string(),
        });
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new());
    let token_url = app
        .token_url
        .clone()
        .unwrap_or_else(|| format!("https://{shop_domain}/admin/oauth/access_token"));
    let response = client
        .post(token_url)
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "client_credentials"),
            ("client_id", app.client_id.as_str()),
            ("client_secret", app.client_secret.as_str()),
        ])
        .send()
        .map_err(|err| ShopifyTokenError::Retryable {
            code: "shopify_token_request_failed".to_string(),
            message: err.to_string(),
        })?;
    let status = response.status().as_u16();
    let body = response.json::<Value>().unwrap_or(Value::Null);
    if status == 429 {
        return Err(ShopifyTokenError::RateLimited {
            message: "shopify token endpoint rate limited".to_string(),
        });
    }
    if !(200..300).contains(&status) {
        let error = body.get("error").and_then(Value::as_str).unwrap_or("");
        let description = body
            .get("error_description")
            .and_then(Value::as_str)
            .unwrap_or("");
        let message = if error.is_empty() {
            format!("token endpoint returned {status}")
        } else if description.is_empty() {
            format!("token endpoint returned {status} {error}")
        } else {
            format!("token endpoint returned {status} {error}: {description}")
        };
        return Err(if (500..600).contains(&status) {
            ShopifyTokenError::Retryable {
                code: "shopify_token_status".to_string(),
                message,
            }
        } else {
            ShopifyTokenError::Rejected { message }
        });
    }
    body.get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ShopifyTokenError::Permanent {
            code: "shopify_token_missing".to_string(),
            message: "access_token absent in Shopify token response".to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_client_secret() {
        let app = ShopifyOAuthApp {
            shop_domain: "example.myshopify.com".to_string(),
            client_id: "client-id".to_string(),
            client_secret: "super-secret".to_string(),
            token_url: None,
        };
        let rendered = format!("{app:?}");
        assert!(rendered.contains("client-id"));
        assert!(!rendered.contains("super-secret"));
    }

    #[test]
    fn normalizes_shop_domain() {
        assert_eq!(
            normalize_shop_domain(" https://example.myshopify.com/ "),
            "example.myshopify.com"
        );
    }

    #[test]
    fn fetch_access_token_uses_token_url_override() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0_u8; 2048];
                let _ = stream.read(&mut buf);
                let request = String::from_utf8_lossy(&buf);
                assert!(request.contains("grant_type=client_credentials"));
                assert!(request.contains("client_id=cid"));
                assert!(request.contains("client_secret=cs"));
                let body = r#"{"access_token":"shopify-access-token"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let token = fetch_access_token(&ShopifyOAuthApp {
            shop_domain: "example.myshopify.com".to_string(),
            client_id: "cid".to_string(),
            client_secret: "cs".to_string(),
            token_url: Some(format!("http://{addr}/token")),
        })
        .expect("token from local endpoint");

        assert_eq!(token, "shopify-access-token");
        let _ = server.join();
    }
}
