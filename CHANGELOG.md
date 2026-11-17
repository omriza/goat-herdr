# Changelog

## Unreleased

- herdr-core support: `goat-herdr watch` follows `hc events`, sends the same alerts, and runs the bridge against `hc`. Runs as a user service; see the README section and `docs/herdr-core.md`. The TUI plugin is unchanged.
- The bridge refuses free text while a herdr-core agent waits on a permission prompt, where the Enter after the text would pick "Yes". Buttons and `/keys` answer it. Questions still take free text.
- `error` status (herdr-core only) for `statuses`.
- Per-session mute: `/mute` and `/unmute` in an agent's topic, or `goat-herdr mute [ID]` and `goat-herdr unmute [ID]` (default: the session it runs in). A mute lasts until the session ends; `watch` drops mutes for sessions that ended while it was stopped.
- Outside Herdr, config and state default to `~/.config/goat-herdr` and `~/.local/state/goat-herdr` (or `GOAT_HERDR_CONFIG_DIR` / `GOAT_HERDR_STATE_DIR`), so `test` and `toggle` work from a shell.

## 0.4.0 (2026-09-19)

- Desktop sink: `osascript` banner on macOS, `notify-send` on Linux.

## 0.3.0 (2026-09-19)

- Pushover sink (application token plus user key, priority by state).
- Pushbullet sink (access token).

## 0.2.0 (2026-09-19)

- Discord sink: channel webhook, or bot token plus channel id. Posting only; the bridge design is in `TODO.md`.

## 0.1.1 (2026-09-19)

- `scripts/release.sh` bumps the version, checks, tags, pushes, and publishes the GitHub release.

## 0.1.0 (2026-09-19)

First release.

- Alerts on `pane.agent_status_changed` for the configured states, with host, workspace, agent, pane, and the last lines of the pane on blocked alerts. Debounce per pane and state. Pause and resume with the `toggle` action.
- Sinks: Telegram (HTML, forum topics per agent pane or per workspace), ntfy, Slack incoming webhook, generic JSON webhook, stdout.
- Telegram bridge: a detached daemon that turns topic replies into `herdr agent prompt`, dialog options into buttons, and `/tail`, `/keys`, `/status`, `/agents` into Herdr calls. Input is accepted only from `allowed_user_ids` in the configured chat.
- Secrets in `.env` beside `config.toml`; error text never includes a URL path.
- Linux and macOS.
