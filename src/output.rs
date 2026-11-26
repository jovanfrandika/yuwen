//! Human-readable rendering.

use crate::api::{TweetView, User};
use std::io::IsTerminal;
use std::sync::OnceLock;

fn color() -> bool {
    static C: OnceLock<bool> = OnceLock::new();
    *C.get_or_init(|| std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none())
}

fn paint(code: &str, s: &str) -> String {
    if color() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String {
    paint("1", s)
}
pub fn dim(s: &str) -> String {
    paint("2", s)
}
fn cyan(s: &str) -> String {
    paint("36", s)
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// "2026-11-27T10:15:30.000Z" -> "2026-11-27 10:15"
fn short_time(ts: &str) -> String {
    let t = ts.replace('T', " ");
    t.get(..16).unwrap_or(&t).to_string()
}

pub fn print_tweet(v: &TweetView) {
    let t = &v.tweet;
    let who = match &v.author {
        Some(u) => format!("{} {}", bold(&u.name), cyan(&format!("@{}", u.username))),
        None => bold(t.author_id.as_deref().unwrap_or("unknown")),
    };
    let mut meta = vec![];
    if let Some(ts) = &t.created_at {
        meta.push(short_time(ts));
    }
    meta.push(format!("id {}", t.id));
    if let Some(refs) = &t.referenced_tweets {
        for r in refs {
            let label = match r.kind.as_str() {
                "replied_to" => "reply to",
                "quoted" => "quotes",
                "retweeted" => "repost of",
                k => k,
            };
            meta.push(format!("{label} {}", r.id));
        }
    }
    println!("{who} {}", dim(&format!("· {}", meta.join(" · "))));
    for line in unescape(&t.text).lines() {
        println!("  {line}");
    }
    if let Some(m) = &t.public_metrics {
        println!(
            "  {}",
            dim(&format!(
                "↩ {}  ↻ {}  ♥ {}  ❝ {}",
                m.reply_count, m.retweet_count, m.like_count, m.quote_count
            ))
        );
    }
    println!();
}

pub fn print_tweets(list: &[TweetView]) {
    if list.is_empty() {
        println!("{}", dim("(no posts)"));
    }
    for v in list {
        print_tweet(v);
    }
}

pub fn print_user(u: &User) {
    let mut line = format!("{} {}", bold(&u.name), cyan(&format!("@{}", u.username)));
    if let Some(m) = &u.public_metrics {
        line.push_str(&dim(&format!(
            "  · {} followers · {} following · {} posts",
            m.followers_count, m.following_count, m.tweet_count
        )));
    }
    println!("{line}");
    if let Some(d) = u.description.as_deref().filter(|d| !d.is_empty()) {
        println!("  {}", dim(&unescape(d).replace('\n', " ")));
    }
}

pub fn print_users(list: &[User]) {
    if list.is_empty() {
        println!("{}", dim("(nobody)"));
    }
    for u in list {
        print_user(u);
    }
}
