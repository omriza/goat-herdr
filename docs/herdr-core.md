# herdr-core support

goat-herdr was built for the Herdr TUI (`herdr`). This page maps it onto herdr-core (`hc`), the core the Herdr desktop and phone apps talk to, and records the design that adds it. Everything here was checked against `hc` 0.1.21 on Linux with Claude Code 2.1 as the agent; the TUI side comes from the Herdr 0.9.5 docs and the fixtures in `tests/fixtures`.

## 1. Event model

| | Herdr TUI | herdr-core |
|---|---|---|
| Delivery | one process per event: `[[events]]` hooks run `goat-herdr notify` with `HERDR_PLUGIN_EVENT_JSON` and `HERDR_PLUGIN_CONTEXT_JSON` | one long-lived `hc events` process; one JSON line per change on stdout |
| First output | none; the hook only sees the event | a snapshot: `{"channel":null,"seq":N,"terminals":[Terminal...]}`, live and finished terminals |
| Event line | `{"event":"pane_agent_status_changed","data":{...}}` | `{"jsonrpc":"2.0","method":"event","params":{"seq":N,"terminal":"t9","type":"status","data":{...}}}` |
| Keep-alive | none | `{"method":"heartbeat","params":{"seq":N}}` every 15 s |
| Falling behind | not applicable | `missed`, then a fresh snapshot; drop events whose `seq` is not above it |
| Core restart | the startup hook runs again | the stream ends; reconnect and take the new snapshot |
| Who runs us | Herdr, as a plugin | nobody; a user service (`systemd --user`) or a shell |

### Event kinds, one to one

| TUI hook | hc event | Notes |
|---|---|---|
| `pane.agent_status_changed` | `status` | hc sends `status` on every change of the record, including `msg` updates while working: 44 `status` lines for 12 turns in the capture. A watcher keeps the last state per terminal and acts on a change of `state`, or on a new `msg` while `blocked` (a second, different dialog). |
| `pane.closed` | `finished`, then `removed` | `finished` carries `exit_code`, `reason` (`exited`, `killed`, `holder_lost`...). `removed` follows `hc rm`. |
| `pane.exited` | `finished` | hc has one event for both. |
| (none) | `created` | the full `Terminal`, before the program reports anything |
| (none) | `turn` | `{"state":"blocked","turn":1}`: the status left `working`. Not sent for a dialog at startup (the trust prompt), so `status` is the event to follow. |
| (none) | `labels` | the full new label map; `where` changes when the space or tab is renamed |
| (none) | `title`, `foreground`, `size`, `doc`, `keep`, `lost`, `restarted` | not needed for alerts |
| `pane.created`, `pane.focused`, `workspace.*`, `tab.*` ... | (none) | hc has no workspace or focus model; spaces and tabs are labels set by the app |

A `status` event carries only `{"status":..., "records":{}, "reported":true}`, no name, labels or cwd. A consumer has to keep its own table of terminals, seeded from the snapshot and `created`, updated by `labels`, and dropped on `finished` or `removed`.

### Status values

| TUI `agent_status` | hc `status.state` | |
|---|---|---|
| `idle` | `idle` | same meaning: waiting, seen |
| `working` | `working` | same |
| `blocked` | `blocked` | hc adds `status.kind`: `permission`, `question` or `auth`, and `msg`, the program's own one-liner: `approve Bash: touch made-by-agent/y`, `answer: Which color do you prefer? (Red · Blue)`, `trust this folder` |
| `done` | `done` | same: finished and not yet looked at |
| `unknown` | `null` | hc has no `unknown`; `status` is `null` for a program that never reported, after it exits, and briefly while Claude Code redraws (the capture has `blocked` → `null` → `idle` around the trust dialog) |
| (none) | `error` | new. goat-herdr gets a `Status::Error` so `statuses = ["error"]` works; TUI users never see it |

Words are the program's own (`app` says which program: `claude-code`). Plain programs report nothing and stay `null`.

### Identifiers and naming

| | TUI | hc |
|---|---|---|
| Pane / terminal id | `w3:pD`, per server session | `t9`, stable for the life of the core; `macbookpro/t1` for a terminal on an added machine |
| Agent name | `display_agent`, else `agent` (`claude`) | `name` if set (`--name goat-test`), else `status.app` (`claude-code`), else the program (`foreground.name`) |
| Workspace | context `workspace_label` (`[1] goat-herdr`) | the space part of the `where` label (`goat-herdr / herdr-core` → `goat-herdr`), else the last folder of `cwd` |
| Tab | context `tab_label` | the tab part of `where`; the ids are in `herdr.space` and `herdr.tab` (opaque hashes) |
| Host | `host_label` | `host_label`, or the machine name for `machine/tN` |

`where`, `herdr.space` and `herdr.tab` are set by the Herdr app for terminals it opens. A terminal started with `hc run` from a shell has no labels, so the name and cwd fallbacks matter.

## 2. Command mapping

| `herdr.rs` | TUI call | hc call | Differences |
|---|---|---|---|
| `read_tail(id, n)` | `herdr pane read <id> --lines N --format text` | `hc read <t> --lines N` | `--lines` in hc is history *above* the screen, and the screen is the full headless size (200×120 by default), mostly blank lines. goat-herdr trims trailing blanks and keeps the last N lines. While `blocked` or `working`, hc returns only the screen (`why: "blocked"`), which is where the dialog is anyway. Output is text, ANSI stripped. |
| `agents()` | `herdr agent list` → `{"result":{"agents":[{pane_id, agent, agent_status, cwd}]}}` | `hc ls --json` → `{"terminals":[{address, agent, name, status, where, cwd, state, turn}]}` | hc lists every terminal; agents are the ones with an `agent` or a `status`. `status` is a plain word in `ls`, a record in `get`. |
| `agent_prompt(id, text)` | `herdr agent prompt <id> <text>`; on `agent_blocked`: `pane send-text` + `send-keys Enter` | `hc send <t> <text>`; blocked: `hc get <t>` then, for a `question`, `hc send --force <t> <text>` | see below |
| `send_keys(id, keys)` | `herdr pane send-keys <id> k...` | `hc keys <t> k...` | Same key names (`enter`, `esc`, `tab`, `up`, `ctrl+c`, digits, letters); hc accepts `Enter` too, so the bridge's existing button data works unchanged. hc waits for ~3 s of quiet before pressing. |
| errors | `{"error":{"code":"agent_blocked","message":...}}`, exit 1 | `{"error":{"kind":"blocked","message":"t9 is blocked (permission)","hint":...}}` on stderr, exit 9 | `kind`, not `code`; exit codes 3 not_found, 9 blocked, 10 status_mismatch, 11 finished |

### Answering a blocked agent, verified

Run against a real Claude Code in `hc run --bg --no-parent --name goat-test -- claude`:

| Action | Result |
|---|---|
| `hc keys t9 down enter` on the trust dialog | trusted, then `idle` |
| `hc keys t9 1` on `approve Bash:` | approved; the command ran, turn ended `done`. A digit picks and confirms. |
| `hc keys t9 1 enter` (what today's buttons send) | approved; the extra Enter lands in the empty input box and does nothing |
| `hc keys t9 Enter` | picks the highlighted option (option 1, Yes) |
| `hc send t9 hello` while blocked | refused: exit 9, `kind: blocked`, nothing typed |
| `hc send --force t9 "use the name zz instead"` on a **permission** dialog | **approved the command**. The text went nowhere; its Enter confirmed the default, Yes. |
| `hc keys t9 3` on an AskUserQuestion dialog (`3. Type something.`), then `hc send --force t9 green` | answered with `green` |

The fourth-to-last row matters. The TUI bridge does the same thing on `agent_blocked` (send-text, then Enter), so a plain reply typed while a permission dialog is open approves it. With hc, `status.kind` tells the two apart, so the hc backend refuses free text on a `permission` dialog and says to use the buttons or `/keys`; on a `question` it types with `--force`. The TUI path is left as it is (see open questions).

`hc send` also waits for a person's unsent draft in that terminal (up to 60 s, `draft_wait`), so a bridge reply can sit while someone is typing on the desktop.

Three more things the live runs turned up:

- **Claude Code reports `blocked` before it draws the dialog.** A read right after the `status` event can catch the spinner (`✶ Pontificating…`) and no options. The hc backend re-reads the screen until two reads 300 ms apart match, at most about 2 s; in testing that took 0.3 to 0.9 s, and every alert carried the dialog. The TUI infers `blocked` from the screen, so it does not have this race.
- **Tagging.** `hc send` run with `HC_TERMINAL_ID` set (inside an hc terminal) delivers `<hc from="t8">...</hc>`, which the agent reads as a note from a parent terminal. The hc backend removes `HC_TERMINAL_ID` from its `hc` calls, so chat text arrives as typed wherever `watch` runs.
- **Flag-looking text.** A reply such as `-v please` would be taken as an option; the backend passes `--` before the address and text.

## 3. Architecture options

### (a) Backward compatible: a second backend and a `watch` daemon

- `src/backend.rs`: a `Backend` trait with the four calls above and the `AgentInfo` row. `herdr::PluginEnv` implements it by calling its existing methods; `hc::Hc` implements it with the `hc` calls.
- `bridge.rs` takes `&dyn Backend` instead of `&PluginEnv`. Command parsing, topics, buttons, `target_pane` do not change.
- `main.rs`: the body of `notify` after the alert is built (pause, debounce, tail, deliver) becomes one function that `notify` and `watch` both call.
- `src/hc.rs`: the hc backend, the `hc events` line parser, and the terminal table that turns lines into status transitions.
- `goat-herdr watch [--hc PATH]`: runs `hc events`, feeds transitions through the shared pipeline, and runs the bridge threads in the same process. Reconnects when the stream ends. Meant for a `systemd --user` unit.
- Config and state directories: `HERDR_PLUGIN_CONFIG_DIR` / `HERDR_PLUGIN_STATE_DIR` when Herdr sets them, as now; otherwise `GOAT_HERDR_CONFIG_DIR` / `GOAT_HERDR_STATE_DIR`, else `~/.config/goat-herdr` and `~/.local/state/goat-herdr`. `test` and `toggle` then work from a shell too.
- `Status::Error` for hc's `error`.

Cost: about 1,500 lines added, including tests, captured fixtures and docs; about 600 of them are new non-test code. No new config keys; one subcommand and one flag.

Risk to TUI users: low. The manifest, `notify`, `bridge`, `test`, `toggle`, config keys and state files are unchanged. The bridge's TUI calls go through the same `PluginEnv` methods. The only TUI-visible change is that `test`/`toggle` no longer fail outside Herdr. All 52 existing tests stay as they are.

Code shape: one more module per concern, no `if tui {} else {}` branches outside the two backend impls.

### (b) Daemon for both backends

Replace the per-event hooks with one daemon for both. The TUI has no CLI stream, but its socket API has `events.subscribe` (newline-delimited JSON on `~/.config/herdr/herdr.sock`, `events_lost` on overflow, resnapshot with `session.snapshot`). The `[[startup]]` hook would start the daemon; `[[events]]` and `notify` go away.

Cost: the above plus a socket client for the TUI (connect, subscribe, resubscribe on `events_lost`, reconcile with a snapshot), and a rewrite of the TUI's lifecycle: Herdr does not supervise plugin daemons and says startup hooks are for one-shot commands, so the daemon has to manage itself the way `bridge --detach` does today. About 1,000 lines changed.

Risk to TUI users: high. Alerts stop if the daemon dies (today each event is its own process), the manifest changes, and `min_herdr_version` has to rise to whatever exposes `events.subscribe`. It would need testing on a real Herdr TUI, which this machine does not have.

Code shape: one event loop, no hook entry point, debounce and topic state in memory instead of lock files. Cleaner, but the TUI gain is small: hooks already work.

### (c) Adapter only

A `goat-herdr watch` that turns each hc line into a fake `HERDR_PLUGIN_EVENT_JSON` and runs `goat-herdr notify`. No changes elsewhere, but the bridge still calls `herdr`, so Telegram can alert but not answer. Fails the goal.

### Recommendation

(a). It is additive and keeps the code clean: the backend split is one trait with two impls, the shared pipeline removes the only place that would otherwise be duplicated, and no config gains a mode switch. (b) is the right shape only if the TUI hooks are to be retired, and that is the upstream maintainer's call.

## 4. Verified end to end

`goat-herdr watch` against the real `hc` on this machine, a real Claude Code, and a local stand-in for the Telegram Bot API (`api_url` pointed at it; it records every call and serves scripted updates):

- A permission prompt became one blocked alert in a new per-agent topic, with the dialog in the tail and `1. Yes` to `4. No` as buttons.
- The `1. Yes` button ran the command; a `done` alert followed in the same topic.
- A plain-text reply on the next permission prompt was refused with the reason; nothing was typed and the file was not created. A message from a user outside `allowed_user_ids` was ignored.
- `4. No` declined the command. A plain-text reply to the idle agent prompted it.
- An AskUserQuestion dialog: the `3. Type something.` button, then the reply `green`, answered it.
- `/status`, `/agents`, `/tail 6` answered in the topic.
- `hc kill` + `hc rm` closed the topic.
- Killing `watch`'s `hc events` child: `watch` reconnected and the next prompt alerted.

## 5. Open questions for upstream

- Free text on a TUI permission dialog approves it (same mechanism as above). The TUI gives no dialog kind; a fix could refuse free text on `blocked` unless a free-text button was pressed first. Not changed here.
- `per-agent` topics on hc are keyed by `tN`, which lives as long as the core. That is longer than a TUI pane id, so topics churn less.
- `error` is not in the default `statuses`. hc users may want `["blocked", "done", "error"]`.
