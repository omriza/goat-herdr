# Fixtures

Captured from Herdr 0.9.1 on macOS with a hook that prints `HERDR_PLUGIN_EVENT_JSON` and `HERDR_PLUGIN_CONTEXT_JSON`. The status change came from `herdr pane report-agent --state blocked`; the close came from `herdr pane close`.

`hc/` is from herdr-core (`hc` 0.1.21) on Linux with Claude Code 2.1, in a terminal started with `hc run --bg --no-parent --name goat-test -- claude`:

- `snapshot.json`: the first line of `hc events`, cut to two terminals (a finished shell and a live agent opened by the Herdr app, with its `where` and `herdr.*` labels); the agent's launch prompt is replaced by `claude`.
- `events.jsonl`: every line for that one terminal from start to `hc rm`, plus heartbeats. It covers the trust dialog, four Bash permission prompts (answered with `hc keys t 1`, `1 enter`, `Enter`, and `send --force`), an AskUserQuestion dialog answered through its free-text option, a `labels` change, `hc kill` and `hc rm`.
- `get_blocked.json`, `read_blocked.json` (`hc read --lines 30 --json`), `ls.json`: the same agent, later, waiting on a permission prompt.
