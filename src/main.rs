mod api;
mod auth;
mod output;

use anyhow::{bail, Context, Result};
use api::{Api, Exclude, TweetView};
use clap::{Args, Parser, Subcommand};
use std::io::{IsTerminal, Read, Write};

#[derive(Parser)]
#[command(name = "yuwen", version, about = "Post, read, edit and delete on X (Twitter) from your terminal")]
struct Cli {
    /// Print raw JSON instead of formatted output
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Log in, log out, or check who you are
    Auth {
        #[command(subcommand)]
        cmd: AuthCmd,
    },

    /// Create a post. Reads from stdin if TEXT is omitted
    Post {
        text: Option<String>,
        /// Post as a reply to this post id/URL
        #[arg(long)]
        reply_to: Option<String>,
        /// Quote this post id/URL
        #[arg(long)]
        quote: Option<String>,
    },

    /// Reply to a post (yours, a follower's, anyone's)
    Reply { id: String, text: Option<String> },

    /// Show a single post
    Show { id: String },

    /// Edit one of your posts. X's API has no edit, so this posts the new
    /// text (keeping reply/quote context) and then deletes the original
    Edit {
        id: String,
        text: Option<String>,
        /// Skip the confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },

    /// Delete one or more of your posts
    Delete {
        #[arg(required = true)]
        ids: Vec<String>,
        /// Skip the confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },

    /// Like a post
    Like { id: String },
    /// Remove a like
    Unlike { id: String },
    /// Repost a post
    Retweet { id: String },
    /// Undo a repost
    Unretweet { id: String },

    /// Your own posts
    Me(TimelineArgs),

    /// Your home feed (accounts you follow, newest first)
    Feed(TimelineArgs),

    /// Posts mentioning you
    Mentions {
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },

    /// Posts by a specific user
    Tweets {
        username: String,
        #[command(flatten)]
        t: TimelineArgs,
    },

    /// List followers (yours or USERNAME's), optionally with their latest posts
    Followers(PeopleArgs),

    /// List accounts followed (by you or USERNAME), optionally with their latest posts
    Following(PeopleArgs),
}

#[derive(Subcommand)]
enum AuthCmd {
    /// Log in through the browser (OAuth 2.0 + PKCE)
    Login {
        /// OAuth 2.0 client id from the X developer portal
        #[arg(long, env = "YUWEN_CLIENT_ID")]
        client_id: Option<String>,
        /// Client secret (only for "confidential" apps)
        #[arg(long, env = "YUWEN_CLIENT_SECRET", hide_env_values = true)]
        client_secret: Option<String>,
        /// Local port for the callback: http://127.0.0.1:<port>/callback
        #[arg(long, default_value_t = 8765)]
        port: u16,
    },
    /// Revoke the token and forget local credentials
    Logout,
    /// Show the logged-in account
    Status,
}

#[derive(Args)]
struct TimelineArgs {
    /// How many posts to fetch
    #[arg(short = 'n', long, default_value_t = 20)]
    limit: usize,
    /// Hide replies
    #[arg(long)]
    no_replies: bool,
    /// Hide reposts
    #[arg(long)]
    no_retweets: bool,
}

impl TimelineArgs {
    fn exclude(&self) -> Exclude {
        Exclude { replies: self.no_replies, retweets: self.no_retweets }
    }
}

#[derive(Args)]
struct PeopleArgs {
    /// Whose list to fetch (defaults to you)
    username: Option<String>,
    /// How many accounts
    #[arg(short = 'n', long, default_value_t = 50)]
    limit: usize,
    /// Also fetch each account's latest posts and show them merged, newest first
    #[arg(long)]
    tweets: bool,
    /// Posts per account when using --tweets
    #[arg(long, default_value_t = 3)]
    per_user: usize,
}

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

/// Accept a bare id or an x.com / twitter.com status URL.
fn post_id(s: &str) -> String {
    let s = s.trim();
    if let Some(i) = s.find("/status/") {
        let rest = &s[i + 8..];
        return rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    }
    s.to_string()
}

fn text_or_stdin(text: Option<String>) -> Result<String> {
    let text = match text {
        Some(t) if t != "-" => t,
        _ => {
            if std::io::stdin().is_terminal() {
                eprintln!("Type your post, then press Ctrl-D:");
            }
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf.trim_end().to_string()
        }
    };
    if text.trim().is_empty() {
        bail!("post text is empty");
    }
    Ok(text)
}

fn confirm(prompt: &str) -> Result<bool> {
    // Read from the terminal even if stdin was used for the post text.
    #[cfg(windows)]
    let tty = std::fs::File::open("CONIN$");
    #[cfg(not(windows))]
    let tty = std::fs::File::open("/dev/tty");
    let mut answer = String::new();
    eprint!("{prompt} [y/N] ");
    std::io::stderr().flush()?;
    match tty {
        Ok(f) => {
            std::io::BufRead::read_line(&mut std::io::BufReader::new(f), &mut answer)?;
        }
        Err(_) => bail!("no terminal to confirm on; pass --yes"),
    }
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn show_tweets(json: bool, list: &[TweetView]) -> Result<()> {
    if json {
        print_json(&list)
    } else {
        output::print_tweets(list);
        Ok(())
    }
}

fn require_own(api: &Api, v: &TweetView) -> Result<()> {
    if v.tweet.author_id.as_deref() != Some(api.creds.user_id.as_str()) {
        let who = v.author.as_ref().map(|u| format!("@{}", u.username)).unwrap_or_default();
        bail!("post {} belongs to {who}; you can only edit or delete your own posts", v.tweet.id);
    }
    Ok(())
}

fn run(cli: Cli) -> Result<()> {
    let json = cli.json;
    match cli.cmd {
        Cmd::Auth { cmd } => return run_auth(cmd, json),

        Cmd::Post { text, reply_to, quote } => {
            let text = text_or_stdin(text)?;
            let api = Api::load()?;
            let id = api.create_tweet(
                &text,
                reply_to.as_deref().map(post_id).as_deref(),
                quote.as_deref().map(post_id).as_deref(),
            )?;
            report_created(&api, json, &id)?;
        }

        Cmd::Reply { id, text } => {
            let text = text_or_stdin(text)?;
            let api = Api::load()?;
            let new_id = api.create_tweet(&text, Some(&post_id(&id)), None)?;
            report_created(&api, json, &new_id)?;
        }

        Cmd::Show { id } => {
            let api = Api::load()?;
            let v = api.tweet(&post_id(&id))?;
            show_tweets(json, std::slice::from_ref(&v))?;
        }

        Cmd::Edit { id, text, yes } => {
            let text = text_or_stdin(text)?;
            let api = Api::load()?;
            let old = api.tweet(&post_id(&id))?;
            require_own(&api, &old)?;
            if !yes {
                output::print_tweet(&old);
                eprintln!("will become:\n  {}\n", text.replace('\n', "\n  "));
                eprintln!("{}", output::dim("(X has no edit API: the original is deleted and its likes/replies are lost)"));
                if !confirm("Replace this post?")? {
                    bail!("cancelled");
                }
            }
            let refs = old.tweet.referenced_tweets.clone().unwrap_or_default();
            let find = |k: &str| refs.iter().find(|r| r.kind == k).map(|r| r.id.clone());
            // Post first so a failure never loses the original.
            let new_id = api.create_tweet(&text, find("replied_to").as_deref(), find("quoted").as_deref())?;
            api.delete_tweet(&old.tweet.id)
                .with_context(|| format!("posted {new_id} but failed to delete the original {}", old.tweet.id))?;
            if !json {
                eprintln!("replaced {} → {new_id}", old.tweet.id);
            }
            report_created(&api, json, &new_id)?;
        }

        Cmd::Delete { ids, yes } => {
            let api = Api::load()?;
            let mut deleted = vec![];
            for raw in ids {
                let id = post_id(&raw);
                let v = api.tweet(&id)?;
                require_own(&api, &v)?;
                if !yes {
                    output::print_tweet(&v);
                    if !confirm("Delete this post?")? {
                        eprintln!("skipped {id}");
                        continue;
                    }
                }
                api.delete_tweet(&id)?;
                if !json {
                    println!("deleted {id}");
                }
                deleted.push(id);
            }
            if json {
                print_json(&serde_json::json!({ "deleted": deleted }))?;
            }
        }

        Cmd::Like { id } => simple(json, "liked", &id, |a, i| a.like(i))?,
        Cmd::Unlike { id } => simple(json, "unliked", &id, |a, i| a.unlike(i))?,
        Cmd::Retweet { id } => simple(json, "reposted", &id, |a, i| a.retweet(i))?,
        Cmd::Unretweet { id } => simple(json, "un-reposted", &id, |a, i| a.unretweet(i))?,

        Cmd::Me(t) => {
            let api = Api::load()?;
            let list = api.user_tweets(&api.creds.user_id.clone(), t.limit, t.exclude())?;
            show_tweets(json, &list)?;
        }

        Cmd::Feed(t) => {
            let api = Api::load()?;
            show_tweets(json, &api.home_timeline(t.limit, t.exclude())?)?;
        }

        Cmd::Mentions { limit } => {
            let api = Api::load()?;
            show_tweets(json, &api.mentions(limit)?)?;
        }

        Cmd::Tweets { username, t } => {
            let api = Api::load()?;
            let id = api.resolve_id(Some(&username))?;
            show_tweets(json, &api.user_tweets(&id, t.limit, t.exclude())?)?;
        }

        Cmd::Followers(p) => people(json, p, true)?,
        Cmd::Following(p) => people(json, p, false)?,
    }
    Ok(())
}

fn report_created(api: &Api, json: bool, id: &str) -> Result<()> {
    if json {
        return print_json(&serde_json::json!({ "id": id }));
    }
    let url = format!("https://x.com/{}/status/{id}", api.creds.username);
    println!("posted {id}  {}", output::dim(&url));
    Ok(())
}

fn simple(json: bool, verb: &str, id: &str, f: impl Fn(&Api, &str) -> Result<()>) -> Result<()> {
    let api = Api::load()?;
    let id = post_id(id);
    f(&api, &id)?;
    if json {
        print_json(&serde_json::json!({ "ok": true, "id": id }))
    } else {
        println!("{verb} {id}");
        Ok(())
    }
}

fn people(json: bool, p: PeopleArgs, followers: bool) -> Result<()> {
    let api = Api::load()?;
    let uid = api.resolve_id(p.username.as_deref())?;
    let users = if followers { api.followers(&uid, p.limit)? } else { api.following(&uid, p.limit)? };

    if !p.tweets {
        return if json { print_json(&users) } else {
            output::print_users(&users);
            Ok(())
        };
    }

    // One request per account: mind your rate limits.
    let mut all = vec![];
    for (i, u) in users.iter().enumerate() {
        if !json && std::io::stderr().is_terminal() {
            eprint!("\r{}", output::dim(&format!("fetching posts {}/{}…", i + 1, users.len())));
        }
        match api.user_tweets(&u.id, p.per_user.max(1), Exclude::default()) {
            Ok(mut list) => {
                list.truncate(p.per_user);
                all.extend(list);
            }
            Err(e) => eprintln!("\nskipping @{}: {e:#}", u.username),
        }
    }
    if !json && std::io::stderr().is_terminal() {
        eprint!("\r\x1b[2K");
    }
    // ISO-8601 timestamps sort lexicographically.
    all.sort_by(|a, b| b.tweet.created_at.cmp(&a.tweet.created_at));
    show_tweets(json, &all)
}

fn run_auth(cmd: AuthCmd, json: bool) -> Result<()> {
    match cmd {
        AuthCmd::Login { client_id, client_secret, port } => {
            let existing = auth::load().ok().flatten();
            let client_id = client_id
                .or_else(|| existing.as_ref().map(|c| c.client_id.clone()))
                .context(
                    "no client id. Create an app at https://developer.x.com, enable OAuth 2.0 \
                     (type: Native App), add http://127.0.0.1:8765/callback as a callback URL, \
                     then run `yuwen auth login --client-id <ID>` or set YUWEN_CLIENT_ID",
                )?;
            let client_secret = client_secret.or_else(|| existing.and_then(|c| c.client_secret));

            let http = api::http_client()?;
            let creds = auth::login(&http, &client_id, client_secret.as_deref(), port)?;
            let mut api = Api::with_creds(http, creds);
            let me = api.me()?;
            api.creds.user_id = me.id.clone();
            api.creds.username = me.username.clone();
            auth::save(&api.creds)?;
            println!("logged in as @{}", me.username);
        }
        AuthCmd::Logout => {
            if let Some(creds) = auth::load()? {
                let http = api::http_client()?;
                if let Err(e) = auth::revoke(&http, &creds) {
                    eprintln!("warning: could not revoke token: {e:#}");
                }
            }
            if auth::remove()? {
                println!("logged out");
            } else {
                println!("not logged in");
            }
        }
        AuthCmd::Status => {
            let api = Api::load()?;
            let me = api.me()?;
            if json {
                print_json(&me)?;
            } else {
                output::print_user(&me);
                println!("{}", output::dim(&format!("credentials: {}", auth::config_dir()?.join("credentials.json").display())));
            }
        }
    }
    Ok(())
}
