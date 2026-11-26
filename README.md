# yuwen

A small X (Twitter) CLI written in Rust. Post, read, edit and delete your posts, and read
your feed, followers, and following from the terminal.

## Install

```sh
cargo install --path .
```

Requires Rust 1.85+ (`rustup update`).

## Setup

1. Create an app at <https://developer.x.com>.
2. In **User authentication settings**, enable **OAuth 2.0**, choose type **Native App**
   (public client), and add the callback URL `http://127.0.0.1:8765/callback`.
3. Copy the **OAuth 2.0 Client ID** and log in:

```sh
yuwen auth login --client-id <CLIENT_ID>   # or export YUWEN_CLIENT_ID
yuwen auth status
```

Credentials (access + refresh token) are stored with `0600` permissions in your config
directory (`~/Library/Application Support/yuwen` on macOS, `~/.config/yuwen` on Linux, or
`$YUWEN_CONFIG_DIR`). Tokens refresh automatically.

> Reading timelines needs an X API tier that allows reads. The Free tier is mostly
> write-only.

## Usage

```sh
# Create
yuwen post "hello from the terminal"
echo "multi-line\npost" | yuwen post
yuwen reply 1234567890 "nice"              # ids or x.com URLs both work
yuwen post "look at this" --quote https://x.com/someone/status/123

# Read
yuwen me -n 10                             # your posts
yuwen feed -n 30 --no-retweets             # home timeline
yuwen mentions
yuwen tweets @someone
yuwen show 1234567890
yuwen followers                            # list your followers
yuwen following --tweets -n 20 --per-user 2  # latest posts from 20 accounts you follow
yuwen followers @someone --tweets

# Update
yuwen edit 1234567890 "fixed typo"         # see note below
yuwen like 123 / unlike 123 / retweet 123 / unretweet 123

# Delete
yuwen delete 1234567890 [more ids...] [-y]
```

Add `--json` to any command for machine-readable output.

### About `edit`

The X API v2 has no edit endpoint. `yuwen edit` posts the new text first (keeping the
original's reply/quote target), then deletes the original. The new post has a new id, and
the original's likes and replies don't carry over. You can only edit or delete your own
posts.

### About `--tweets`

`followers/following --tweets` makes one API request per account, so keep `-n` small to
stay inside rate limits.
