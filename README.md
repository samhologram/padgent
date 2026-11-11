# Padgent

A light, native macOS menu-bar app that shows your AI coding agents and chats on a
**Logitech MX Creative Keypad** — one key per conversation, lit by what each one is doing —
and on screen as a Liquid Glass keypad. Written in Rust.

![Sample Padgent app showing demonstration sessions](docs/images/padgent-app.png)

*Sample app with demonstration sessions.*

![Sample Padgent menu-bar keypad](docs/images/padgent-menu-bar.png)

*Sample menu-bar keypad with demonstration sessions.*

- **Agents:** Claude Code, Codex, Antigravity, ZCode, Kimi Code, plus the ChatGPT and Claude
  desktop apps. Add any other agent by importing an `.orbagent.json` file, or follow any desktop
  app's conversations through macOS Accessibility (no logins in Padgent).
- **Keypad:** eight session keys and a hub key that counts what's waiting; previous/next page
  buttons; hold a key 0.6 s to archive its conversation until work resumes; hold both page buttons 1.5 s to hand the keypad
  back to Logi Options+. Each key's light follows the session's state (thinking, tool, writing,
  waiting for approval or an answer, done, error, compacting). Sessions idle for more than
  30 minutes move to page 2 or later, leaving page 1 for active and recent sessions.
- **Tap to open:** opens the exact conversation in its app, or the terminal tab a Claude Code
  session runs in (Terminal, iTerm2).
- **Menu bar:** left-click the icon to open the live 3×3 keypad; right-click for session
  previews, settings, hand-over and quit. The icon shows a waiting dot and count.
- **Settings:** colours, motion, key order, recent sessions, hand-over, launch at login, model
  roles for Claude Code (Haiku for the swarm, Sonnet for code, Opus for orchestration).

## Build

Requires Rust (stable) on macOS 14 or later.

```sh
cargo build --release          # the program
./bundle.sh                    # builds target/Padgent.app
./dmg.sh                       # builds an Apple Silicon disk image in target/
cargo run -- --screenshot shot.png   # renders the window to a PNG and exits
cargo run -- --screenshot-menu-bar menu.png  # renders the menu-bar keypad and exits
```

On first use macOS asks you to allow Padgent in **Accessibility** (to follow desktop apps),
**Input Monitoring** (to read the keypad) and **Automation** (to select a terminal tab).

## Connecting agents

- **Claude Code:** Agents › Claude Code › *Install hooks* adds a small hook to
  `~/.claude/settings.json` that reports each session's state to Padgent on `127.0.0.1:47800`.
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

Padgent is not affiliated with Logitech, Anthropic or OpenAI. Logitech, MX and
Options+ are trademarks of Logitech; Claude is a trademark of Anthropic; Codex and ChatGPT are
trademarks of OpenAI.

## License

MIT — see `LICENSE`.

### Quotas

Press the previous page button from the first session page to see quota observations on the keypad, the main window and the menu-bar keypad. The page buttons cycle through quotas and session pages. Quota keys are informational and do not open or clear sessions.

Codex readings come from recent local session logs and reflect the most recent reported rate-limit event, not a live account query. A passed reset time or an old observation does not imply the allowance has refreshed. Only quota windows that have actually been observed are shown; a platform or quota type with no observation has no tile. Claude Desktop quota readings come from a visible usage summary when Accessibility access is enabled. Readings expire after 15 minutes or their reported reset time, whichever comes first; expired readings show Unknown until refreshed.
