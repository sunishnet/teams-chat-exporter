//! Microsoft Entra ID sign-in: OAuth 2.0 authorization code flow with PKCE,
//! using the system browser and a loopback (http://localhost:<port>) redirect -
//! the same way `Connect-MgGraph` signs in interactively.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Microsoft public client ID for Microsoft Graph Command Line Tools.
/// Consent requirements remain subject to the user's organization policy.
pub const DEFAULT_CLIENT_ID: &str = "14d82eec-204b-4c2f-b7e8-296a70dab67e";
pub const DEFAULT_TENANT: &str = "organizations";
pub const AUTHORITY: &str = "https://login.microsoftonline.com";
pub const SCOPES: &str = "https://graph.microsoft.com/Chat.Read https://graph.microsoft.com/User.ReadBasic.All offline_access openid profile";

const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Debug)]
pub struct AuthConfig {
    pub tenant: String,
    pub client_id: String,
}

impl AuthConfig {
    fn token_url(&self) -> String {
        format!("{AUTHORITY}/{}/oauth2/v2.0/token", self.tenant)
    }
    fn authorize_url(&self) -> String {
        format!("{AUTHORITY}/{}/oauth2/v2.0/authorize", self.tenant)
    }
}

#[derive(Clone, Debug)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Instant,
}

impl Token {
    pub fn needs_refresh(&self) -> bool {
        self.expires_at
            .checked_duration_since(Instant::now())
            .map(|left| left < Duration::from_secs(300))
            .unwrap_or(true)
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct TokenError {
    error: String,
    error_description: Option<String>,
}

fn random_urlsafe(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    getrandom::fill(&mut b).expect("OS random number generator unavailable");
    URL_SAFE_NO_PAD.encode(b)
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn parse_token_response(resp: reqwest::blocking::Response) -> Result<Token, String> {
    let status = resp.status();
    let text = resp
        .text()
        .map_err(|e| format!("Token response unreadable: {e}"))?;
    if !status.is_success() {
        if let Ok(e) = serde_json::from_str::<TokenError>(&text) {
            let desc = e.error_description.unwrap_or_default();
            let first_line = desc.lines().next().unwrap_or("").to_string();
            return Err(format!("{}: {}", e.error, first_line));
        }
        return Err(format!("Token request failed ({status}): {text}"));
    }
    let t: TokenResponse =
        serde_json::from_str(&text).map_err(|e| format!("Unexpected token response: {e}"))?;
    Ok(Token {
        access_token: t.access_token,
        refresh_token: t.refresh_token,
        expires_at: Instant::now() + Duration::from_secs(t.expires_in.unwrap_or(3600)),
    })
}

/// Use the refresh token to get a fresh access token.
pub fn refresh(
    cfg: &AuthConfig,
    http: &reqwest::blocking::Client,
    refresh_token: &str,
) -> Result<Token, String> {
    let resp = http
        .post(cfg.token_url())
        .form(&[
            ("client_id", cfg.client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("scope", SCOPES),
        ])
        .send()
        .map_err(|e| format!("Could not reach login.microsoftonline.com: {e}"))?;
    let mut t = parse_token_response(resp)?;
    if t.refresh_token.is_none() {
        t.refresh_token = Some(refresh_token.to_string());
    }
    Ok(t)
}

fn respond_html(req: tiny_http::Request, title: &str, body: &str) {
    let page = format!(
        "<!DOCTYPE html><html><head><meta charset='utf-8'><title>{title}</title>\
         <style>body{{font-family:'Segoe UI',Arial,sans-serif;display:flex;align-items:center;\
         justify-content:center;height:100vh;margin:0;background:#f5f5f5;color:#242424}}\
         div{{background:#fff;padding:32px 40px;border-radius:8px;box-shadow:0 2px 8px rgba(0,0,0,.1)}}\
         h1{{font-size:20px;margin:0 0 8px;color:#464775}}</style></head>\
         <body><div><h1>{title}</h1><p>{body}</p></div></body></html>"
    );
    let header =
        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
            .unwrap();
    let _ = req.respond(tiny_http::Response::from_string(page).with_header(header));
}

/// Interactive sign-in. Opens the default browser and waits for the redirect.
pub fn sign_in_interactive(
    cfg: &AuthConfig,
    http: &reqwest::blocking::Client,
    cancel: &AtomicBool,
    log: &dyn Fn(String),
) -> Result<Token, String> {
    // Listen on 127.0.0.1 and (if possible) [::1] on the same port, because the
    // browser may resolve "localhost" to either.
    let v4 = tiny_http::Server::http("127.0.0.1:0")
        .map_err(|e| format!("Could not start local sign-in listener: {e}"))?;
    let port = v4
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .ok_or("Local listener has no port")?;
    let mut servers = vec![Arc::new(v4)];
    if let Ok(v6) = tiny_http::Server::http(format!("[::1]:{port}")) {
        servers.push(Arc::new(v6));
    }

    let redirect_uri = format!("http://localhost:{port}");
    let verifier = random_urlsafe(48);
    let state = random_urlsafe(16);
    let auth_url = url::Url::parse_with_params(
        &cfg.authorize_url(),
        &[
            ("client_id", cfg.client_id.as_str()),
            ("response_type", "code"),
            ("redirect_uri", redirect_uri.as_str()),
            ("response_mode", "query"),
            ("scope", SCOPES),
            ("state", state.as_str()),
            ("code_challenge", pkce_challenge(&verifier).as_str()),
            ("code_challenge_method", "S256"),
            ("prompt", "select_account"),
        ],
    )
    .map_err(|e| e.to_string())?;

    log("Opening your browser to sign in...".into());
    if open::that(auth_url.as_str()).is_err() {
        log(format!(
            "Could not open a browser automatically. Open this link to sign in:\n{auth_url}"
        ));
    }

    // Funnel requests from all listeners into one channel.
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<tiny_http::Request>();
    for s in &servers {
        let s = Arc::clone(s);
        let tx = tx.clone();
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if let Ok(Some(req)) = s.recv_timeout(Duration::from_millis(250)) {
                    if tx.send(req).is_err() {
                        break;
                    }
                }
            }
        });
    }
    drop(tx);
    struct StopOnDrop(Arc<AtomicBool>);
    impl Drop for StopOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let _guard = StopOnDrop(Arc::clone(&stop));

    let started = Instant::now();
    let code = loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("Sign-in cancelled.".into());
        }
        if started.elapsed() > SIGN_IN_TIMEOUT {
            return Err("Sign-in timed out (5 minutes). Please try again.".into());
        }
        let req = match rx.recv_timeout(Duration::from_millis(300)) {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("Local sign-in listener stopped unexpectedly.".into())
            }
        };
        let full = format!("http://localhost{}", req.url());
        let Ok(u) = url::Url::parse(&full) else {
            let _ = req.respond(tiny_http::Response::empty(400));
            continue;
        };
        let q: std::collections::HashMap<String, String> = u.query_pairs().into_owned().collect();

        if let Some(err) = q.get("error") {
            let desc = q.get("error_description").cloned().unwrap_or_default();
            respond_html(
                req,
                "Sign-in failed",
                &crate::util::html_escape(&format!("{err}: {desc}")),
            );
            let first = desc.lines().next().unwrap_or("").to_string();
            return Err(format!("{err}: {first}"));
        }
        if let Some(code) = q.get("code") {
            if q.get("state") != Some(&state) {
                respond_html(req, "Sign-in failed", "State mismatch - please try again.");
                return Err("Sign-in response had an unexpected state value.".into());
            }
            respond_html(
                req,
                "Signed in",
                "You can close this tab and return to Teams Chat Exporter.",
            );
            break code.clone();
        }
        // favicon.ico etc.
        let _ = req.respond(tiny_http::Response::empty(404));
    };

    log("Completing sign-in...".into());
    let resp = http
        .post(cfg.token_url())
        .form(&[
            ("client_id", cfg.client_id.as_str()),
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("code_verifier", verifier.as_str()),
            ("scope", SCOPES),
        ])
        .send()
        .map_err(|e| format!("Could not reach login.microsoftonline.com: {e}"))?;
    parse_token_response(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc7636_example() {
        // RFC 7636 Appendix B
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
}
