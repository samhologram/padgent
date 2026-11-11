# Padgent mechanics

Padgent shows agent sessions on screen and can drive the Logitech MX Creative Keypad.
The physical keypad driver is experimental. Settings, imported agent definitions and
per-agent preferences remain in the existing AgentPad configuration directory for compatibility.

## Sessions and actions

Sessions can come from local agent records, hooks, or desktop apps followed through macOS
Accessibility. The application displays each session’s activity, age and latest message.
Tapping a key opens its conversation or terminal; holding a key for 0.6 seconds moves it to the end of the keypad pages until work resumes. Archives are kept in memory
and reset when Padgent restarts. The two page buttons navigate pages;
holding both for 1.5 seconds releases the keypad to Logi Options+.

## Display and keypad

Session lights use a core, halo, rings, moving lights, ripple and grain. Their animation
varies by activity and follows the motion and brightness settings. The hub summarizes
sessions and those waiting for attention. The experimental HID driver takes exclusive access,
reads key and page-button events, and sends JPEG images to the keypad.

See `src/model.rs`, `src/field.rs` and `src/hardware.rs` for the current implementation and
`mechanics.json` for the recorded parameters.
