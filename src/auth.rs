//! OAuth 2.0 (Authorization Code + PKCE) for X, plus credential storage.

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

const AUTHORIZE_URL: &str = "https://x.com/i/oauth2/authorize";
const TOKEN_URL: &str = "https://api.x.com/2/oauth2/token";
const REVOKE_URL: &str = "https://api.x.com/2/oauth2/revoke";

pub const SCOPES: &str =
    "tweet.read tweet.write users.read follows.read like.read like.write offline.access";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Credentials {
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Unix seconds.
    pub expires_at: u64,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub username: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

pub fn config_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("YUWEN_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let base = dirs::config_dir().ok_or_else(|| anyhow!("could not find a config directory"))?;
    Ok(base.join("yuwen"))
}

fn creds_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("credentials.json"))
}

pub fn load() -> Result<Option<Credentials>> {
    let path = creds_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(Some(serde_json::from_str(&raw).context("credentials file is corrupt; run `yuwen auth login`")?))
}

pub fn save(creds: &Credentials) -> Result<()> {
    let path = creds_path()?;
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&path).with_context(|| format!("writing {}", path.display()))?;
    f.write_all(serde_json::to_string_pretty(creds)?.as_bytes())?;
    Ok(())
}

pub fn remove() -> Result<bool> {
    let path = creds_path()?;
    if path.exists() {
        std::fs::remove_file(path)?;
        return Ok(true);
    }
    Ok(false)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn random_b64(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

fn token_request(
    http: &Client,
    client_id: &str,
    client_secret: Option<&str>,
    form: &[(&str, &str)],
) -> Result<TokenResponse> {
    let mut form = form.to_vec();
    let mut req = http.post(TOKEN_URL);
    match client_secret {
        Some(secret) => req = req.basic_auth(client_id, Some(secret)),
        None => form.push(("client_id", client_id)),
    }
    let resp = req.form(&form).send().context("token request failed")?;
    let status = resp.status();
    let body = resp.text()?;
    if !status.is_success() {
        bail!("token endpoint returned {status}: {body}");
    }
    serde_json::from_str(&body).context("unexpected token response")
}

/// Run the browser-based login flow. Returns credentials without user info filled in.
pub fn login(http: &Client, client_id: &str, client_secret: Option<&str>, port: u16) -> Result<Credentials> {
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let verifier = random_b64(48);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_b64(16);

    let mut url = Url::parse(AUTHORIZE_URL)?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("scope", SCOPES)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");

    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("could not listen on 127.0.0.1:{port} (try --port)"))?;

    eprintln!("Opening your browser to authorize yuwen. If it doesn't open, visit:\n\n  {url}\n");
    let _ = open::that(url.as_str());

    let code = wait_for_code(&listener, &state)?;
    let tok = token_request(
        http,
        client_id,
        client_secret,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", &redirect_uri),
            ("code_verifier", &verifier),
        ],
    )?;

    Ok(Credentials {
        client_id: client_id.to_string(),
        client_secret: client_secret.map(str::to_string),
        access_token: tok.access_token,
        refresh_token: tok.refresh_token,
        expires_at: now() + tok.expires_in.unwrap_or(7200),
        user_id: String::new(),
        username: String::new(),
    })
}

fn wait_for_code(listener: &TcpListener, expected_state: &str) -> Result<String> {
    loop {
        let (mut stream, _) = listener.accept()?;
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line)?;
        // e.g. "GET /callback?code=...&state=... HTTP/1.1"
        let path = line.split_whitespace().nth(1).unwrap_or("/");
        let url = Url::parse(&format!("http://127.0.0.1{path}"))?;
        if url.path() != "/callback" {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
            continue;
        }
        let get = |k: &str| url.query_pairs().find(|(key, _)| key == k).map(|(_, v)| v.into_owned());

        let (ok, msg, result) = if let Some(err) = get("error") {
            (false, format!("Authorization failed: {err}"), Err(anyhow!("authorization denied: {err}")))
        } else if get("state").as_deref() != Some(expected_state) {
            (false, "State mismatch. Please retry.".into(), Err(anyhow!("OAuth state mismatch")))
        } else if let Some(code) = get("code") {
            (true, "yuwen is authorized. You can close this tab.".into(), Ok(code))
        } else {
            (false, "Missing code.".into(), Err(anyhow!("callback had no code")))
        };

        let body = format!(
            "<!doctype html><meta charset=utf-8><title>yuwen</title>\
             <body style=\"font-family:system-ui;padding:3rem;background:#111;color:#eee\">\
             <h2>{}</h2><p>{msg}</p></body>",
            if ok { "Done" } else { "Oops" }
        );
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        return result;
    }
}

/// Refresh the access token if it expires within a minute. Returns true if creds changed.
pub fn ensure_fresh(http: &Client, creds: &mut Credentials) -> Result<bool> {
    if creds.expires_at > now() + 60 {
        return Ok(false);
    }
    let refresh = creds
        .refresh_token
        .clone()
        .ok_or_else(|| anyhow!("access token expired; run `yuwen auth login`"))?;
    let tok = token_request(
        http,
        &creds.client_id,
        creds.client_secret.as_deref(),
        &[("grant_type", "refresh_token"), ("refresh_token", &refresh)],
    )
    .context("could not refresh token; run `yuwen auth login`")?;
    creds.access_token = tok.access_token;
    if tok.refresh_token.is_some() {
        creds.refresh_token = tok.refresh_token;
    }
    creds.expires_at = now() + tok.expires_in.unwrap_or(7200);
    Ok(true)
}

pub fn revoke(http: &Client, creds: &Credentials) -> Result<()> {
    let mut form = vec![("token", creds.access_token.as_str()), ("token_type_hint", "access_token")];
    let mut req = http.post(REVOKE_URL);
    match creds.client_secret.as_deref() {
        Some(secret) => req = req.basic_auth(&creds.client_id, Some(secret)),
        None => form.push(("client_id", creds.client_id.as_str())),
    }
    let resp = req.form(&form).send()?;
    if !resp.status().is_success() {
        bail!("revoke returned {}", resp.status());
    }
    Ok(())
}
