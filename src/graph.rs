//! Minimal Microsoft Graph client: auth header, paging, throttling retries, token refresh.

use crate::auth::{self, AuthConfig, Token};
use reqwest::blocking::{Client, Response};
use reqwest::StatusCode;
use serde_json::Value;
use std::fmt;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

pub const GRAPH: &str = "https://graph.microsoft.com/v1.0";

#[derive(Debug, Clone)]
pub struct GraphError {
    pub status: Option<u16>,
    pub message: String,
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.status {
            Some(s) => write!(f, "{} (HTTP {s})", self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

impl GraphError {
    fn new(status: Option<u16>, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

pub type GResult<T> = Result<T, GraphError>;

pub fn http_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(180))
        .user_agent(concat!("TeamsChatExporter/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("HTTP client")
}

pub struct Graph {
    http: Client,
    base: String,
    token: Mutex<Token>,
    /// None = static token (tests): never refreshed.
    auth: Option<AuthConfig>,
}

impl Graph {
    pub fn new(http: Client, token: Token, auth: Option<AuthConfig>) -> Self {
        Self {
            http,
            base: GRAPH.to_string(),
            token: Mutex::new(token),
            auth,
        }
    }

    /// Point at a different Graph root (used by tests with a mock server).
    #[allow(dead_code)]
    pub fn with_base(mut self, base: impl Into<String>) -> Self {
        self.base = base.into();
        self
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    fn map_url(&self, url: &str) -> String {
        if self.base != GRAPH {
            if let Some(rest) = url.strip_prefix(GRAPH) {
                return format!("{}{}", self.base, rest);
            }
        }
        url.to_string()
    }

    fn access_token(&self, force_refresh: bool) -> GResult<String> {
        let mut t = self.token.lock().unwrap();
        if let Some(cfg) = &self.auth {
            if force_refresh || t.needs_refresh() {
                if let Some(rt) = t.refresh_token.clone() {
                    match auth::refresh(cfg, &self.http, &rt) {
                        Ok(nt) => *t = nt,
                        Err(e) if force_refresh || t.needs_refresh() => {
                            return Err(GraphError::new(
                                None,
                                format!("Session expired - please sign in again ({e})"),
                            ))
                        }
                        Err(_) => {}
                    }
                }
            }
        }
        Ok(t.access_token.clone())
    }

    fn send(&self, url: &str, headers: &[(&str, &str)]) -> GResult<Response> {
        let url = self.map_url(url);
        let mut attempt = 0u32;
        let mut refreshed = false;
        loop {
            let token = self.access_token(false)?;
            let mut req = self.http.get(&url).bearer_auth(token);
            for (k, v) in headers {
                req = req.header(*k, *v);
            }
            let resp = match req.send() {
                Ok(r) => r,
                Err(e) => {
                    attempt += 1;
                    if attempt <= 3 {
                        std::thread::sleep(Duration::from_secs(2u64.pow(attempt)));
                        continue;
                    }
                    return Err(GraphError::new(None, format!("Network error: {e}")));
                }
            };
            let status = resp.status();
            if status.is_success() {
                return Ok(resp);
            }
            if status == StatusCode::UNAUTHORIZED && !refreshed && self.auth.is_some() {
                refreshed = true;
                self.access_token(true)?;
                continue;
            }
            if matches!(status.as_u16(), 429 | 503 | 504) && attempt < 6 {
                attempt += 1;
                let wait = resp
                    .headers()
                    .get("Retry-After")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .unwrap_or_else(|| 2u64.pow(attempt))
                    .clamp(1, 120);
                std::thread::sleep(Duration::from_secs(wait));
                continue;
            }
            let code = status.as_u16();
            let body = resp.text().unwrap_or_default();
            let msg = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .map(String::from)
                })
                .unwrap_or_else(|| {
                    status
                        .canonical_reason()
                        .unwrap_or("Request failed")
                        .to_string()
                });
            return Err(GraphError::new(Some(code), msg));
        }
    }

    pub fn get_json(&self, url: &str) -> GResult<Value> {
        self.get_json_with(url, &[])
    }

    pub fn get_json_with(&self, url: &str, headers: &[(&str, &str)]) -> GResult<Value> {
        let resp = self.send(url, headers)?;
        resp.json::<Value>()
            .map_err(|e| GraphError::new(None, format!("Invalid JSON from Graph: {e}")))
    }

    /// Follow @odata.nextLink until done. `on_page` gets the running item count and
    /// can return false to stop early (cancel).
    pub fn get_paged(
        &self,
        url: &str,
        mut on_page: impl FnMut(usize) -> bool,
    ) -> GResult<Vec<Value>> {
        self.get_paged_with_items(url, |_, count| on_page(count))
    }

    /// Follow @odata.nextLink and make each page available as soon as it arrives.
    pub fn get_paged_with_items(
        &self,
        url: &str,
        mut on_page: impl FnMut(&[Value], usize) -> bool,
    ) -> GResult<Vec<Value>> {
        let mut out = Vec::new();
        let mut next = Some(url.to_string());
        while let Some(u) = next.take() {
            let v = self.get_json(&u)?;
            let start = out.len();
            if let Some(items) = v.get("value").and_then(|x| x.as_array()) {
                out.extend(items.iter().filter(|i| !i.is_null()).cloned());
            }
            if !on_page(&out[start..], out.len()) {
                return Err(GraphError::new(None, "Cancelled"));
            }
            next = v
                .get("@odata.nextLink")
                .and_then(|x| x.as_str())
                .map(String::from);
        }
        Ok(out)
    }

    /// Download binary content to a file. Returns the response content type.
    pub fn download(&self, url: &str, path: &Path) -> GResult<String> {
        let resp = self.send(url, &[])?;
        let ct = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = resp
            .bytes()
            .map_err(|e| GraphError::new(None, format!("Download failed: {e}")))?;
        std::fs::write(path, &bytes).map_err(|e| {
            GraphError::new(None, format!("Could not write {}: {e}", path.display()))
        })?;
        Ok(ct)
    }
}
