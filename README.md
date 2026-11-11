# AgentPad

A light, native macOS menu-bar app that shows your AI coding agents and chats on a
**Logitech MX Creative Keypad** — one key per conversation, lit by what each one is doing —
and on screen as a Liquid Glass keypad. Written in Rust.

- **Agents:** Claude Code, Codex, Antigravity, ZCode, Kimi Code, plus the ChatGPT and Claude
  desktop apps. Add any other agent by importing an `.orbagent.json` file, or follow any desktop
  app's conversations through macOS Accessibility (no logins in AgentPad).
- **Keypad:** eight session keys and a hub key that counts what's waiting; previous/next page
  buttons; hold a key 0.6 s for its focus view; hold both page buttons 1.5 s to hand the keypad
  back to Logi Options+. Each key's light follows the session's state (thinking, tool, writing,
  waiting for approval or an answer, done, error, compacting).
- **Tap to open:** opens the exact conversation in its app, or the terminal tab a Claude Code
  session runs in (Terminal, iTerm2).
- **Menu bar:** waiting dot and count, and a live preview of every session with its last message.
- **Settings:** colours, motion, key order, recent sessions, hand-over, launch at login, model
  roles for Claude Code (Haiku for the swarm, Sonnet for code, Opus for orchestration).

## Build

Requires Rust (stable) on macOS 14 or later.

```sh
cargo build --release          # the program
./bundle.sh                    # builds AgentPad.app and installs it in ~/Applications
cargo run -- --screenshot shot.png   # renders the window to a PNG and exits
```

On first use macOS asks you to allow AgentPad in **Accessibility** (to follow desktop apps),
**Input Monitoring** (to read the keypad) and **Automation** (to select a terminal tab).

## Connecting agents

- **Claude Code:** Agents › Claude Code › *Install hooks* adds a small hook to
  `~/.claude/settings.json` that reports each session's state to AgentPad on `127.0.0.1:47800`.
- **Anything else:** point its hooks or scripts at `POST http://127.0.0.1:47800/hook/<agent-id>`
  (hook JSON on stdin), or post a state directly:

  ```sh
  curl -X POST http://127.0.0.1:47800/event/my.agent \
    -d '{"session":"demo","state":"thinking","title":"My task","text":"Working on it"}'
  ```

States: `starting`, `idle`, `thinking`, `tool`, `streaming`, `waitingPermission`,
`waitingQuestion`, `done`, `error`, `compacting`, `offline`.

## Agent files

```json
{
  "format": "agentpad.agent", "version": 1,
  "id": "com.example.myagent", "name": "My Agent", "badge": "MA",
  "hue": 200, "style": "orb",
  "app_name": "My Agent", "watch_app": "com.example.myagent.desktop",
  "state_colours": { "working": "#4aa3ff" }
}
```

## Notes

`MECHANICS.md` documents the app’s behaviour (timings, colours, keypad
protocol). The physical keypad driver is experimental.

AgentPad is not affiliated with Logitech, Anthropic or OpenAI. Logitech, MX and
Options+ are trademarks of Logitech; Claude is a trademark of Anthropic; Codex and ChatGPT are
trademarks of OpenAI.

## License

MIT — see `LICENSE`.
