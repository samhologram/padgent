# Padgent mechanics

Padgent shows agent sessions on screen and can drive the Logitech MX Creative Keypad.
The physical keypad driver is experimental. Settings, imported agent definitions and
per-agent preferences remain in the existing AgentPad configuration directory for compatibility.

## Sessions and actions

Sessions can come from local agent records, hooks, or desktop apps followed through macOS
Accessibility. The application displays each session’s activity, age and latest message.
Tapping a key opens its conversation or terminal. Inactive conversations are acknowledged
and stay in place at 45% intensity, including their text. New or resumed sessions use free
first-page slots, then replace acknowledged tiles if needed, leaving bright neighbours
fixed. Replaced tiles remain accessible on later pages. Resumed activity restores full
intensity.
Holding a key for 0.6 seconds opens its observed steps, one per key. Back occupies the
bottom-right key. The two page buttons navigate conversation or step pages.
The local ordering resets when Padgent restarts. Holding both page buttons for 1.5 seconds
releases the keypad to Logi Options+.

## Display and keypad

Session lights use a core, halo, rings, moving lights, ripple and grain. Their animation
varies by activity and follows the motion and brightness settings. The hub summarizes
sessions and those waiting for attention. The experimental HID driver takes exclusive access,
reads key and page-button events, and sends JPEG images to the keypad.

See `src/model.rs`, `src/field.rs` and `src/hardware.rs` for the current implementation and
`mechanics.json` for the recorded parameters.
