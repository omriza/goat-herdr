# goat-herdr

A [Herdr](https://herdr.dev) plugin that alerts you when an agent needs you. Telegram, Discord, ntfy, Pushover, Pushbullet, Slack, any JSON webhook, or a desktop banner. From Telegram you can answer the agent.

- An agent goes `blocked` or `done`: one message, with the host, workspace, agent, pane, and the last lines of its terminal.
- Telegram forum topics give each agent pane its own thread.
- Reply in that thread and the text goes to the agent. Buttons under a blocked alert answer its dialog.
- Works with the Herdr TUI as a plugin, and with [herdr-core](#herdr-core) (`hc`, behind the Herdr desktop and phone apps) as a user service.

## Install

```sh
herdr plugin install shindakun/goat-herdr
```

Needs `cargo`; the install step builds the binary. Linux and macOS.

## Configure

Config lives in `$(herdr plugin config-dir shindakun.goat-herdr)/config.toml`. With no file, alerts print to the plugin log (`herdr plugin log list --plugin shindakun.goat-herdr`).

```toml
[alerts]
statuses = ["blocked", "done"]   # agent states that fire; also idle, working, unknown
debounce_secs = 5                # drop a repeat of the same pane and state inside this window
tail_lines = 30                  # pane output attached to blocked alerts; 0 disables
host_label = "mac-mini"          # default: hostname

[[sinks]]
type = "telegram"
bot_token_env = "TELEGRAM_BOT_TOKEN"   # or bot_token = "123:abc"
chat_id = -1001234567890
topics = "per-agent"                   # none | per-agent | per-workspace
bridge = true                          # two-way; needs allowed_user_ids
allowed_user_ids = [123456789]         # Telegram user ids the bridge obeys

[[sinks]]
type = "ntfy"
url = "https://ntfy.sh/your-topic"
token_env = "NTFY_TOKEN"               # optional Bearer token

[[sinks]]
type = "slack"
webhook_url_env = "SLACK_WEBHOOK_URL"  # or webhook_url = "https://hooks.slack.com/services/..."

[[sinks]]
type = "webhook"
url = "https://example.com/hook"       # or url_env = "MY_HOOK_URL"

[[sinks]]
type = "discord"
webhook_url_env = "DISCORD_WEBHOOK_URL"   # or bot_token_env = "DISCORD_BOT_TOKEN" with channel_id = "..."

[[sinks]]
type = "pushover"
app_token_env = "PUSHOVER_TOKEN"       # application token from pushover.net/apps/build
user_key_env = "PUSHOVER_USERKEY"      # your user key from the Pushover dashboard

[[sinks]]
type = "pushbullet"
token_env = "PUSHBULLET_TOKEN"         # access token from pushbullet.com/#settings/account

[[sinks]]
type = "desktop"
sound = "Ping"                         # macOS system sound; optional

[[sinks]]
type = "stdout"                        # prints to the plugin log; the default when no sink is set
```

Any `*_env` key names a variable that is read from `.env` next to `config.toml` first, then from the environment. Keep secrets there:

```sh
TELEGRAM_BOT_TOKEN=123456:abc...
SLACK_WEBHOOK_URL=https://hooks.slack.com/services/...
DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/...
PUSHOVER_TOKEN=a...
PUSHOVER_USERKEY=u...
PUSHBULLET_TOKEN=o....
```

Every alert starts with the same line, so one chat can carry several machines:

```text
🟥 BLOCKED claude · goat-herdr · mac-mini
pane w3:p3
<last 30 lines of the pane>
```

## Sinks

### Telegram

Create a bot with @BotFather and put its token in `.env`. Send the bot a message, or add it to a group and post there, then read `chat.id` from `https://api.telegram.org/bot<token>/getUpdates`.

`topics` needs a supergroup with Topics turned on and the bot as an admin with Manage Topics.

- `per-agent`: one topic per agent pane, named `claude · goat-herdr · mac-mini · w3:p16`. A reply in the topic reaches that pane and no other. The topic is created on the first alert, closed when the pane closes, and reopened if the pane alerts again. Under herdr-core (`watch`) the topic is named for the session instead, `tavern · Fix the login bug`: the Herdr space, the `--name` if one was given, and the title the agent sets (Claude Code's is a summary of the conversation). The topic is renamed when that changes, and a new session that reuses an id gets a new topic.
- `per-workspace`: one topic per host and workspace, shared by every agent in it.
- `none`: everything in the chat root.

`api_url` overrides `https://api.telegram.org` for a self-hosted Bot API server.

### Discord

One message per alert in a channel: bold headline, pane line, tail in a code block, within Discord's 2,000-character limit. Two ways in:

- Webhook: channel settings → Integrations → Webhooks → New Webhook → Copy Webhook URL. `webhook_url` or `webhook_url_env`. No bot needed.
- Bot: create an application at discord.com/developers, open Bot, reset the token, and put it in `.env` as `bot_token_env` names it. Invite the bot to your server with the `bot` scope and the permissions it needs (View Channel, Send Messages, Send Messages in Threads):

  ```text
  https://discord.com/oauth2/authorize?client_id=<APPLICATION_ID>&scope=bot&permissions=309237648384
  ```

  `<APPLICATION_ID>` is on the app's General Information page. Adding the app from the Developer Portal without that URL installs it with no bot member, and every post fails with `403 Missing Access`. Then set `channel_id` to the channel's id (Copy Channel ID in Discord with Developer Mode on, or the last number in the channel's URL).

Posting only. A Discord bridge is on the [TODO](TODO.md).

### ntfy

One POST per alert to the topic URL. `Title` is the headline, `Priority` is `urgent` for blocked and `default` for done, `Tags` is the status square. `token` or `token_env` adds `Authorization: Bearer` for a protected topic. One-way.

### Pushover

One message per alert to every device on the account. Needs two secrets: an application token, created at pushover.net/apps/build, and the user key shown on the Pushover dashboard. Blocked alerts are priority 1 (high), done is 0, other states are -1 (quiet). The title is the headline; the body is monospace and cut from the top to Pushover's 1,024-character limit. One-way.

### Pushbullet

One note per alert to every device on the account. Needs the access token from pushbullet.com, Settings → Account → Create Access Token. The title is the headline; the body is the plain rendering. One-way.

### Desktop

A notification banner on the machine running Herdr. macOS: `osascript` with `display notification`; the banner comes from Script Editor, so allow Script Editor under System Settings → Notifications if nothing shows. `sound` names a system sound. Linux: `notify-send`, urgency `critical` for blocked, `normal` for done, `low` otherwise. Title is the headline; body is the pane and the last two lines of the tail. No network.

### Slack

At api.slack.com/apps create an app from a manifest with the `incoming-webhook` bot scope, open Incoming Webhooks, add a webhook to a channel, and copy the URL. One POST per alert in mrkdwn with the tail in a code block. The URL is the credential. One-way.

### Generic webhook

One JSON POST per alert, `Content-Type: application/json`. Any 2xx is delivered. Pane closes are not sent. No headers or auth; a secret rides in the URL through `url_env`. One-way.

```json
{
  "source": "goat-herdr",
  "status": "blocked",
  "host": "mac-mini",
  "workspace": "goat-herdr",
  "agent": "claude",
  "pane_id": "w3:p18",
  "headline": "BLOCKED claude · goat-herdr · mac-mini",
  "tail": "last lines of the pane, or null",
  "text": "🟥 BLOCKED claude · goat-herdr · mac-mini\npane w3:p18\n---\nlast lines of the pane"
}
```

`status` is `blocked`, `done`, `idle`, `working`, or `unknown`. `tail` is set on blocked alerts when `tail_lines` is above zero. `text` is the plain rendering ntfy and the plugin log use.

### Adding one

A sink is one file under `src/sink/` with a `name` and a `send`, plus one match arm in `src/sink/mod.rs` and a config struct. `ntfy.rs` is the template. Services with the same shape, an HTTP POST and a token: Mattermost (Slack-compatible webhook), Microsoft Teams (workflow webhook), Gotify, Matrix (a room webhook bot). Anything the `apprise` CLI covers can be reached by shelling out to it, the way the desktop sink shells out.

## Bridge

With `bridge = true` and your Telegram user id in `allowed_user_ids`, the startup hook runs a daemon that polls the bot. In an agent's topic:

| You send | It does |
|---|---|
| plain text | prompts the agent; when the agent is waiting on a dialog, types it into the dialog's text field |
| a button under a blocked alert | picks that numbered option, or sends `Enter` or `Esc`, or posts the pane `Tail` |
| `/tail [n]` | posts the last n lines of the pane |
| `/keys y Enter` | sends key presses |
| `/status` | this agent's row from `herdr agent list` |
| `/mute`, `/unmute` | stops or resumes this agent's alerts; the topic still takes commands |
| `/agents` | every live agent (works in any topic) |
| `/help` | the command list |

Blocked alerts show the dialog's numbered options as buttons. A free-text option (`Type something`, `Other`) selects the field; your next text reply fills it.

Every input and every dropped update is one line in `bridge.log` in the plugin's state directory.

## Actions

| Action | Does |
|---|---|
| `shindakun.goat-herdr.test` | sends a test alert to every sink |
| `shindakun.goat-herdr.toggle` | pauses or resumes alerts |
| `shindakun.goat-herdr.bridge` | restarts the bridge daemon |

Bind one in your Herdr config:

```toml
[[keys.command]]
key = "prefix+shift+a"
type = "plugin_action"
command = "shindakun.goat-herdr.toggle"
description = "toggle agent alerts"
```

## herdr-core

herdr-core (`hc`) runs agents in terminals that outlive any window; the Herdr desktop and phone apps talk to it. It has no plugin hooks, so goat-herdr runs beside it as one long-running process:

```sh
goat-herdr watch [--hc /path/to/hc]
```

`watch` follows `hc events`, sends an alert for every status change your `statuses` list, and runs the bridge in the same process against `hc`. Buttons, replies, `/tail`, `/keys`, `/status` and `/agents` work as on the TUI. The ids are hc addresses (`t7`, or `machine/t7` for an added machine, whose name becomes the alert's host). The workspace is the Herdr app's space (from the `where` label), else the agent's folder; the agent is its `--name`, else the program's own name (`claude-code`).

Config and state live in `~/.config/goat-herdr/` and `~/.local/state/goat-herdr/` (`$XDG_CONFIG_HOME`, `$XDG_STATE_HOME`, or `GOAT_HERDR_CONFIG_DIR` and `GOAT_HERDR_STATE_DIR`). `config.toml` and `.env` take the same keys as above. hc also reports `error`; add it to `statuses` to hear about it.

Install and run it as a user service:

```sh
cargo install --git https://github.com/shindakun/goat-herdr
mkdir -p ~/.config/goat-herdr ~/.config/systemd/user
$EDITOR ~/.config/goat-herdr/config.toml        # sinks as above; .env beside it, mode 600
goat-herdr test                                  # one test alert to every sink
curl -fsSLo ~/.config/systemd/user/goat-herdr.service \
  https://raw.githubusercontent.com/shindakun/goat-herdr/main/contrib/goat-herdr.service
systemctl --user daemon-reload
systemctl --user enable --now goat-herdr
journalctl --user -u goat-herdr -f
```

The unit expects `goat-herdr` in `~/.cargo/bin` and `hc` in `~/.local/bin`; edit `ExecStart` if yours differ. `toggle` pauses alerts for `watch` too.

To silence one session, send `/mute` in its topic, or run `goat-herdr mute` inside it (it reads `HC_TERMINAL_ID`; `goat-herdr mute t7` from anywhere). `/unmute` or `goat-herdr unmute` turns it back on. A mute lasts until the session ends, even one that ends while `watch` is stopped: on start it unmutes ids that are no longer live. The same works in the TUI, where the id is the pane id.

Differences from the TUI:

- An agent that was already blocked when `watch` started is not announced; everything after is. When the event stream drops (a core restart), `watch` reconnects and alerts on what changed in between.
- hc says what a dialog is. While an agent waits on a permission prompt, a plain-text reply is refused, because the Enter after it would pick the highlighted "Yes"; use the buttons or `/keys`. A question still takes text after you pick its free-text option.
- Two different dialogs in a row each alert, even inside `debounce_secs`.
- Run one bridge per bot token. If the TUI plugin and `watch` share a machine, give them different bots, or turn `bridge` off in one.

The event and command mapping, and the design choices behind it, are in [docs/herdr-core.md](docs/herdr-core.md).

## Security

The bridge lets a chat message drive a terminal on your machine.

- Only Telegram user ids in `allowed_user_ids` are accepted. Everyone else is logged as `ignored user <id>` and dropped: nothing reaches Herdr, nothing is replied. An empty list accepts nobody.
- Only updates from the configured `chat_id` are read.
- Telegram user ids are not secret. This is "only this account", not a password. Keep the group private and add only people you would hand a shell to.
- The bot token is the credential. Anyone with it can read the group, post as the bot, and use the bot's admin rights. Keep it in `.env` in the plugin config directory, mode 600, never in the plugin root or a repo. The plugin does not write it to logs or error messages. If it leaks, revoke it in @BotFather.
- Slack webhook URLs and secret webhook URLs are credentials in the same way. Errors name the host only, never the path.
- Blocked alerts carry the last lines of the agent's terminal. Whatever is on screen goes to every configured sink.
- Network destinations are the configured sinks: `api.telegram.org`, `discord.com`, your ntfy server, `api.pushover.net`, `api.pushbullet.com`, `hooks.slack.com`, your webhook URL. Nothing else.

## Develop

```sh
git clone https://github.com/shindakun/goat-herdr
cd goat-herdr
cargo build --release
herdr plugin link .
herdr plugin action invoke shindakun.goat-herdr.test
herdr plugin log list --plugin shindakun.goat-herdr
```

`make check` runs fmt, clippy, tests, `cargo audit`, and markdownlint. CI runs the same on Linux and macOS. The linked plugin executes `target/release/goat-herdr`, so rebuild release before testing in Herdr. Design and milestones: [docs/PLAN.md](docs/PLAN.md).

To release: add a `## X.Y.Z (date)` section to `CHANGELOG.md`, commit it, then `scripts/release.sh X.Y.Z`. The script runs `make check`, bumps `Cargo.toml`, `Cargo.lock`, and `herdr-plugin.toml`, builds release, commits, tags `vX.Y.Z`, pushes, and publishes the GitHub release with the changelog section as notes. The marketplace shows the new version within 30 minutes.

## License

MIT
