//! Thin X API v2 client.

use crate::auth::{self, Credentials};
use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::{Client, RequestBuilder};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

const BASE: &str = "https://api.x.com/2";
const TWEET_FIELDS: &str = "created_at,public_metrics,author_id,conversation_id,referenced_tweets";
const USER_FIELDS: &str = "username,name,description,public_metrics,created_at";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Tweet {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub author_id: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub public_metrics: Option<TweetMetrics>,
    #[serde(default)]
    pub referenced_tweets: Option<Vec<RefTweet>>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct TweetMetrics {
    #[serde(default)]
    pub retweet_count: u64,
    #[serde(default)]
    pub reply_count: u64,
    #[serde(default)]
    pub like_count: u64,
    #[serde(default)]
    pub quote_count: u64,
    #[serde(default)]
    pub impression_count: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RefTweet {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct User {
    pub id: String,
    pub name: String,
    pub username: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub public_metrics: Option<UserMetrics>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct UserMetrics {
    #[serde(default)]
    pub followers_count: u64,
    #[serde(default)]
    pub following_count: u64,
    #[serde(default)]
    pub tweet_count: u64,
}

/// A tweet joined with its author, for display / JSON output.
#[derive(Serialize, Clone, Debug)]
pub struct TweetView {
    #[serde(flatten)]
    pub tweet: Tweet,
    pub author: Option<User>,
}

#[derive(Deserialize)]
struct Page<T> {
    #[serde(default = "Vec::new")]
    data: Vec<T>,
    #[serde(default)]
    includes: Includes,
    #[serde(default)]
    meta: Meta,
}

#[derive(Deserialize, Default)]
struct Includes {
    #[serde(default)]
    users: Vec<User>,
}

#[derive(Deserialize, Default)]
struct Meta {
    next_token: Option<String>,
}

#[derive(Deserialize)]
struct One<T> {
    data: T,
    #[serde(default)]
    includes: Includes,
}

#[derive(Clone, Copy, Default)]
pub struct Exclude {
    pub replies: bool,
    pub retweets: bool,
}

impl Exclude {
    fn param(self) -> Option<String> {
        let mut v = vec![];
        if self.replies {
            v.push("replies");
        }
        if self.retweets {
            v.push("retweets");
        }
        (!v.is_empty()).then(|| v.join(","))
    }
}

pub struct Api {
    http: Client,
    pub creds: Credentials,
}

pub fn http_client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent(concat!("yuwen/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

impl Api {
    /// Load stored credentials, refreshing the access token if needed.
    pub fn load() -> Result<Self> {
        let http = http_client()?;
        let mut creds = auth::load()?.ok_or_else(|| anyhow!("not logged in; run `yuwen auth login`"))?;
        if auth::ensure_fresh(&http, &mut creds)? {
            auth::save(&creds)?;
        }
        Ok(Self { http, creds })
    }

    pub fn with_creds(http: Client, creds: Credentials) -> Self {
        Self { http, creds }
    }

    fn me_id(&self) -> &str {
        &self.creds.user_id
    }

    fn send(&self, req: RequestBuilder) -> Result<Value> {
        let resp = req.bearer_auth(&self.creds.access_token).send().context("request to X failed")?;
        let status = resp.status();
        let reset = resp
            .headers()
            .get("x-rate-limit-reset")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = resp.text()?;
        if status.as_u16() == 429 {
            let when = reset.map(|r| format!(" (resets at unix time {r})")).unwrap_or_default();
            bail!("rate limited by X{when}");
        }
        let value: Value = if body.trim().is_empty() { Value::Null } else { serde_json::from_str(&body).unwrap_or(Value::String(body.clone())) };
        if !status.is_success() {
            let detail = value
                .get("detail")
                .or_else(|| value.get("title"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or(body);
            bail!("X API returned {status}: {detail}");
        }
        // 200 responses can still carry errors with no data (e.g. tweet not found).
        if value.get("data").is_none() {
            if let Some(err) = value.get("errors").and_then(|e| e.get(0)) {
                let msg = err.get("detail").or_else(|| err.get("title")).and_then(Value::as_str).unwrap_or("unknown error");
                bail!("{msg}");
            }
        }
        Ok(value)
    }

    fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T> {
        let v = self.send(self.http.get(format!("{BASE}{path}")).query(query))?;
        serde_json::from_value(v).context("unexpected response shape from X")
    }

    fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.send(self.http.post(format!("{BASE}{path}")).json(&body))
    }

    fn delete(&self, path: &str) -> Result<Value> {
        self.send(self.http.delete(format!("{BASE}{path}")))
    }

    // ---------- users ----------

    pub fn me(&self) -> Result<User> {
        let one: One<User> = self.get("/users/me", &[("user.fields", USER_FIELDS.into())])?;
        Ok(one.data)
    }

    pub fn user_by_username(&self, username: &str) -> Result<User> {
        let username = username.trim_start_matches('@');
        let one: One<User> = self.get(
            &format!("/users/by/username/{username}"),
            &[("user.fields", USER_FIELDS.into())],
        )?;
        Ok(one.data)
    }

    /// Resolve an optional username to a user id (defaults to the logged-in user).
    pub fn resolve_id(&self, username: Option<&str>) -> Result<String> {
        match username {
            Some(u) => Ok(self.user_by_username(u)?.id),
            None => Ok(self.me_id().to_string()),
        }
    }

    pub fn followers(&self, user_id: &str, limit: usize) -> Result<Vec<User>> {
        self.users_paged(&format!("/users/{user_id}/followers"), limit)
    }

    pub fn following(&self, user_id: &str, limit: usize) -> Result<Vec<User>> {
        self.users_paged(&format!("/users/{user_id}/following"), limit)
    }

    fn users_paged(&self, path: &str, limit: usize) -> Result<Vec<User>> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        while out.len() < limit {
            let want = (limit - out.len()).clamp(1, 1000);
            let mut q = vec![("max_results", want.to_string()), ("user.fields", USER_FIELDS.into())];
            if let Some(t) = &token {
                q.push(("pagination_token", t.clone()));
            }
            let page: Page<User> = self.get(path, &q)?;
            out.extend(page.data);
            match page.meta.next_token {
                Some(t) => token = Some(t),
                None => break,
            }
        }
        out.truncate(limit);
        Ok(out)
    }

    // ---------- tweets: read ----------

    pub fn tweet(&self, id: &str) -> Result<TweetView> {
        let one: One<Tweet> = self.get(
            &format!("/tweets/{id}"),
            &[
                ("tweet.fields", TWEET_FIELDS.into()),
                ("expansions", "author_id".into()),
                ("user.fields", USER_FIELDS.into()),
            ],
        )?;
        let author = one.includes.users.into_iter().next();
        Ok(TweetView { tweet: one.data, author })
    }

    pub fn user_tweets(&self, user_id: &str, limit: usize, ex: Exclude) -> Result<Vec<TweetView>> {
        self.tweets_paged(&format!("/users/{user_id}/tweets"), limit, ex, 5)
    }

    pub fn home_timeline(&self, limit: usize, ex: Exclude) -> Result<Vec<TweetView>> {
        self.tweets_paged(&format!("/users/{}/timelines/reverse_chronological", self.me_id()), limit, ex, 1)
    }

    pub fn mentions(&self, limit: usize) -> Result<Vec<TweetView>> {
        self.tweets_paged(&format!("/users/{}/mentions", self.me_id()), limit, Exclude::default(), 5)
    }

    fn tweets_paged(&self, path: &str, limit: usize, ex: Exclude, min_page: usize) -> Result<Vec<TweetView>> {
        let mut out = Vec::new();
        let mut users: HashMap<String, User> = HashMap::new();
        let mut token: Option<String> = None;
        while out.len() < limit {
            let want = (limit - out.len()).clamp(min_page, 100);
            let mut q = vec![
                ("max_results", want.to_string()),
                ("tweet.fields", TWEET_FIELDS.into()),
                ("expansions", "author_id".into()),
                ("user.fields", USER_FIELDS.into()),
            ];
            if let Some(e) = ex.param() {
                q.push(("exclude", e));
            }
            if let Some(t) = &token {
                q.push(("pagination_token", t.clone()));
            }
            let page: Page<Tweet> = self.get(path, &q)?;
            for u in page.includes.users {
                users.insert(u.id.clone(), u);
            }
            out.extend(page.data);
            match page.meta.next_token {
                Some(t) => token = Some(t),
                None => break,
            }
        }
        out.truncate(limit);
        Ok(out
            .into_iter()
            .map(|t| {
                let author = t.author_id.as_ref().and_then(|id| users.get(id)).cloned();
                TweetView { tweet: t, author }
            })
            .collect())
    }

    // ---------- tweets: write ----------

    pub fn create_tweet(&self, text: &str, reply_to: Option<&str>, quote: Option<&str>) -> Result<String> {
        let mut body = json!({ "text": text });
        if let Some(id) = reply_to {
            body["reply"] = json!({ "in_reply_to_tweet_id": id });
        }
        if let Some(id) = quote {
            body["quote_tweet_id"] = json!(id);
        }
        let v = self.post("/tweets", body)?;
        v["data"]["id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("X did not return the new post id"))
    }

    pub fn delete_tweet(&self, id: &str) -> Result<()> {
        let v = self.delete(&format!("/tweets/{id}"))?;
        if v["data"]["deleted"].as_bool() != Some(true) {
            bail!("X did not confirm deletion of {id}");
        }
        Ok(())
    }

    pub fn like(&self, id: &str) -> Result<()> {
        self.post(&format!("/users/{}/likes", self.me_id()), json!({ "tweet_id": id }))?;
        Ok(())
    }

    pub fn unlike(&self, id: &str) -> Result<()> {
        self.delete(&format!("/users/{}/likes/{id}", self.me_id()))?;
        Ok(())
    }

    pub fn retweet(&self, id: &str) -> Result<()> {
        self.post(&format!("/users/{}/retweets", self.me_id()), json!({ "tweet_id": id }))?;
        Ok(())
    }

    pub fn unretweet(&self, id: &str) -> Result<()> {
        self.delete(&format!("/users/{}/retweets/{id}", self.me_id()))?;
        Ok(())
    }
}
