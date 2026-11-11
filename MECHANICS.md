# legacy mechanics sheet (from the original 0.3.12, arm64 Swift build)

Every value below was read from the original program's code or text. Addresses are
virtual addresses in `legacy.app/Contents/MacOS/legacy`. "estimated" means the code
was searched and the exact number wasn't found.

## Keypad geometry — `legacyContracts.KeypadGeometry` (0x10042b24c–0x10042b338)
| value | number | source |
|---|---|---|
| keys | 9 (3 columns × 3 rows) | keyCount 0x10042b258, columns 0x10042b26c, rows 0x10042b280 |
| key image | 118 × 118 px | keySize 0x10042b294; "expected 118x118" string |
| grid origin | x 23, y 6 px | originX 0x10042b2a8, originY 0x10042b2bc |
| key pitch | 158 px | pitch 0x10042b2d0 (keyRect 0x10042b33c) |
| grid rect | 23, 6, 434 × 434 | gridRect 0x10042b2e4 |
| hub key | index 8 (bottom right) | hubKeyIndex 0x10042b320 |
| session keys per page | 8 | sessionKeysPerPage 0x10042b334 |
| largest single-packet JPEG | 4075 bytes | maxSinglePacketJPEGBytes 0x10042b30c |

## Settings defaults — `legacySettingsKey.registrationDefaults` (0x100476f44)
| key | default | note |
|---|---|---|
| hookPort | 47800 | local hook server port |
| launchAtLogin | false | |
| autoTakeOverOnLaunch | true | "Take over the keypad when legacy starts" |
| restoreOnQuit | true | quitting always gives the keypad back to Options+ |
| hideOnClickElsewhere | true | popover |
| targetFPS | 30 | keypad redraw rate |
| brightness | 0.8 | keypad LCD backlight while driven |
| textSpeed | 18 chars/s | "Streamed text types out at 18 characters per second" |
| showBackgroundSessions | false | |
| idleDimMinutes | 10 | then the keypad dims and redraws at 10 fps |
| keyActionsEnabled | true | |
| recencyMinutes | 120 | recent sessions keep a key this long |
| reorderOn | "turns" | keys move only when a turn starts/ends; a new wait moves up at once |
| lockedMode | "all" (all / orbs / off) | LockedMode.rawValue 0x100474970 |
| oneAlarmColour | false | |
| colourKeysBy | "agent" (agent / state) | |
| keyGlyphs, providerAppIcons | true | |
| emptyKeyURL | "" | link opened by a key with no session |
| keypadTheme | "frosted" | KeypadTheme.rawValue 0x1004749dc (only theme) |
| adapterAPIEnabled | false | external agents must be allowed one by one |

## Session states — `SessionActivity.allCases`
empty, starting, idle, thinking, tool, streaming, waitingPermission, waitingQuestion,
done, error, compacting, offline. Tool kinds: shell, edit, read, web, delegate.
Key order: waiting, working, just finished, idle ("Keys sort by state..." string).
Working and waiting sessions always keep a key. Error always stays red.

## Session state rules — `SessionRecord.resolve` (0x100749a48)
In order: no signal for too long → offline; a pending approval → waiting for approval; a pending
question, or a notification mentioning "input", "question" or "elicitation" → waiting for an answer
(any other notification → approval); host disconnected → error; a running tool or sub-agent → tool;
text within the streaming window → writing; busy → thinking; finished within doneDecay → done;
started within startingWindow → starting; otherwise idle.

`SessionTimings` from `SessionTracker.Environment.live` (0x10075703c, constants 0x1009f17b0):
| field | value |
|---|---|
| streamingWindow | 1.5 s |
| doneDecay | 180 s |
| startingWindow | 60 s |
| staleBusyAfter | 900 s |
| staleIdleAfter | 7200 s |
| removeAfter | 43200 s (12 h) |
| subagentForgetAfter | 1800 s |
| sub-agent done decay | 30 s (SubagentSnapshot.doneDecaySeconds 0x1004644d4) |

## Agent colours — `AdapterPaletteSpec.builtInDefault(for:)` (0x100437c28)
Hues are degrees on the OKLCH wheel.
| agent | hue | |
|---|---|---|
| codex | 270 | |
| zcode | 165 | |
| antigravity | 225 | |
| kimi-code | 324 | |
| claude-code / other | 79.85 | default branch; matches the reference palette's thinking orb #d9a441 (OKLCH hue 79.8) |
| attention band (reserved for "waiting") | 18.9 – 55 | attentionExclusion 0x100430688 |
| calm arc (working states) | 80 – 280 | calmArc 0x1004306b4 |
| min colour distance between states | ΔE 10 | proximityDeltaE 0x1004306d8 |
| contrast for marks on a key colour | 4.5 : 1 (WCAG weights 0.2126/0.7152/0.0722) | OrbPalette.generate 0x100438d50 |

Reference palette (OrbPalette.reference, 0x100431480; static colour arrays 0x100b933c8..0x100b93670),
five shades per state, dark → light (stop 2 = lit orb, stop 3 = glow):
| state | ramp |
|---|---|
| starting | 17110a 3d2a16 8a6435 c9a466 ead6a8 |
| idle | 1e170c 5a4620 b89660 efd9a8 fff6e4 |
| thinking | 221408 6b4416 d9a441 f0c46a fff1d0 |
| tool | 221408 6e4816 dfa946 f4cb72 fff4d6 |
| streaming | 241a0c 7c5a22 e0b35a f5d78f fff8e6 |
| waitingPermission | 240c05 6a2612 d9693e f08a5a ffd6b8 (hue 40.8) |
| waitingQuestion | 241005 6e3016 e0703f f29a5e ffdcb8 |
| done | 1c1610 5a4c38 c9b38e efe3cf fffaf0 |
| error | 1e0605 4e100d 9e2a22 c8352b f07a60 (hue 28.5) |
| compacting | 18140f 4a4030 a8966e dccaa0 f4ecd8 |
| offline | 14110f 2c2723 4e463d 6a6157 |
Background 0b0908. Accents: starting e8cf98, idle a39482, thinking e6b45a, tool/streaming f0c46a,
waiting f08a5a. The ramp order is matched to states by colour meaning and the accent tables'
explicit state keys (estimated for starting/idle/compacting). Other agents get these ramps turned
on the OKLCH wheel by (agent hue − 79.85°), keeping lightness and chroma; waiting keeps 40.8°
(turned by the Waiting setting), error stays red, offline grey.

OrbPalette.generate works in OKLab (sRGB transfer 12.92 / 1.055 / 2.4) and uses hue boundaries 30.9°,
45°, 75.9°, 80° and 280°; its per-state lightness/chroma ramps were not recovered (2,483 lines of
register-level output); the rebuild derives them from the reference ramps above instead.

## Key travel — `Travel.position` (0x1005058e4), `planTravels` (0x100500fc8)
When sessions reorder, keys travel to their new slot over 0.8 s with ease-in-out cubic:
p < 0.5 → 4p³, else 1 − (2 − 2p)³ / 2; slot = (index mod 3, index div 3) × pitch 158.

## Keypad hand-over
* Takeover opens the keypad's vendor HID interface exclusively (IOKitHIDTransport.open(seize:)),
  which locks Logi Options+ out; release closes it and Options+ gets the keypad back at once.
* Interface match: Logitech vendor 0x046D, usage page 0xFF43, usage 0x1A10 (isVendorInterface 0x10049cf34).
* Features: 0x1B04 (reprogrammable controls: divert the page buttons), 0x8040 (LCD brightness).
* Page buttons: control IDs 0x01A1 (previous) and 0x01A2 (next) (InputParser.parse 0x1004aff60).
* Key presses: input report 0x13, byte 2 = 0x02, byte 3 = 0x00, byte 4 = 0x01, then pressed key numbers 1–9.
* Image ACK: input report 0x13 / 0x02 / 0x2B; status (0,0|1) ok, (1,1) busy, 0xFB error (ackStatus 0x1004afef0).
* HID++ request: 20 bytes `11 FF <featureIndex> <function<<4 | 0x0E> <16 params>` (HIDPPFrame.request 0x1004afb0c).
* Key image packets (VLPFraming.packets 0x1004af5c0): 4095-byte reports starting `14 FF 02 2B`, byte 4 =
  0x20 | packet counter (low nibble, from 1), +0x80 on the first packet, +0x40 on the last.
  First packet header: `01 <deferUpdate> 01 00 xH xL yH yL wH wL hH hL lenHH lenH lenL`, JPEG from byte 20;
  later packets carry JPEG from byte 5.
* KEEP_ALIVE is HID++ feature 0x0008 (lookup in performTakeover closure 0x1004a4ca4); sendKeepAlive
  (0x1004a4f20) calls function 1 with params `0B B8` (3000 ms, static array 0x100b970d8), repeated by a
  timer every keepAliveInterval (startKeepAlive 0x1004a50f4, 20 ms leeway). Without it the panel
  reverts to the Logi image after 3 s without frames.
* Hold both page buttons 1.5 s → hand back to Options+. Hold a session key 0.6 s → focus view on all nine keys.
* Mac sleep → keypad goes to Options+ and returns on wake.
* Takeover retries (scheduleRetry 0x1004a60c0, closure 0x1004a63dc): delay = min(retryBaseDelay ·
  2^(attempt−1), retryMaxDelay) = 2, 4, 8, 16, 32, 60 s, at most maxAutoRetries = 6; then
  "automatic retries used up (take over again to retry)".
* Image recovery (same closure): silent reopen after 2, 5, 15, 30 s (static array 0x100b97108).
* KeypadDevice.Timing (KeypadDeviceFactory.makeWithDiagnostics 0x1004ac734, constants 0x1009ee8c0):
  keepAliveInterval 1.0 s, connectionSettleInterval 10 s, connectionQuietInterval 2 s,
  initialProbeInterval 1 s, requestTimeout 1 s, releaseRequestTimeout 0.2 s, releaseBudget 0.9 s,
  emergencyRequestTimeout 0.15 s, emergencyLockWait 0.6 s, retryBaseDelay 2 s, retryMaxDelay 60 s,
  maxAutoRetries 6, usbArrivalTimeout 5 s.
* "waiting for Options+ initialization before taking over" (performTakeover 0x1004a21d4): a newly
  plugged keypad is left to Options+ until it has been quiet 2 s (settle limit 10 s).

## Menu bar
* "Dot shows a terracotta dot on the menu bar icon. Dot and count adds the number of waiting sessions."
* Menu: switch the keypad between legacy and Options+, sessions preview, Settings, Quit.

## Settings groups (strings)
Look (theme, colours, brightness, motion) · Keys and sessions (key actions, order, recent sessions,
built-in agent switches) · Menu bar and hand-over (indicators, shortcuts, keypad hand-over) · Agents.
Settings export/import as a versioned JSON document.

## Hand-over on launch
autoTakeOverOnLaunch defaults to true: the rebuild drives the physical keypad by default and takes
it from Logi Options+ when it opens.

## Rebuild additions (not in the original)
* Liquid-glass styling and spring animations on the on-screen keypad.
* Agent definitions export/import as `.orbagent.json`.
* Model roles for Claude: Haiku for swarm, Sonnet for code, Opus for orchestration.

## Accessibility recovery — `AppController.recoverStaleAccessibilityIfNeeded` (0x1000b9350)
When Accessibility isn't trusted, an Accessibility feature is in use (an app is followed), and the
running code signature differs from the one stored under "accessibilityRecoveryRequirement",
the app resets its own entry once (`/usr/bin/tccutil reset Accessibility <bundle id>`,
AccessibilityRecovery.resetOwnEntry 0x100003784), then calls AXIsProcessTrustedWithOptions with the
prompt option (closure 0x1000b9a34) and stores the signature. If tccutil fails it shows the
remove-and-add steps instead. The rebuild uses the CDHash from `codesign -dvvv` as the signature
and AgentPad's bundle id com.agentpad.app.

## Key light field — `legacyParams` (0x10052f400), exit block 0x100531ab4
Each key's light is a field (FP struct: cx, cy, cs, ci, hs, hi, floor, rr, rw, ri, r2r, r2w, r2i,
l1x, l1y, l1s, l1i, l2x, l2y, l2s, l2i, ripple, grain, grainRate) chosen by a switch over the orb
state (empty, offline, idle, starting, thinking, tool, streaming, permission, question, done, error,
compacting; jump table 0x1009f0b40). Defaults: core at (0.5, 0.84) in key units (y up, so the light
sits near the top with the text underneath), core size 0.12, halo 0.36, ring width 0.05, lights
size 0.1, grain rate 0.2. Fixed values traced per state:
| state | values |
|---|---|
| offline | ring r 0.13, w 0.032, i 0.55 (no core) |
| idle | core 0.1, halo 0.3; breath and drift on a 6 s clock |
| starting | halo 0.34; fades in over 1.4 s, ring every 1.6 s |
| thinking | halo i 0.3, light1 i 0.3, light2 size 0.11 i 0.26, grain 0.1 @ 0.22; core pulse 3.2 s (±0.07 around 0.88), lights circle at 5.3 s / 11 s |
| tool | core 0.095, halo 0.32, lights size 0.052 / 0.062 moved by ToolMotion.frame per tool |
| streaming | core 0.13 i 0.9, halo 0.37 i 0.36, ripple 0.2, grain 0.05 @ 0.15 |
| permission | beacon on a 1.6 s clock: core i 0.72–0.98, halo i 0.45, two rings 0.12→0.45 half a period apart |
| question | halo 0.34; softer bob, ring 0.12–0.24 |
| done | core 0.13, halo 0.38, grain 0.03 @ 0.04; ring burst over 1.4 s |
| error | halo 0.34, floor 0.18 (red flood), grain 0.06 @ 0.5 |
| compacting | core 0.11, halo i 0.26; ring closes in every 2.4 s |
Clock-driven shapes (sin/frac curves) are rebuilt from each case's constants (estimated forms).
The rebuild draws this field behind a frosted glass veil with a specular edge ("more liquid glass").

## Opening a conversation
legacy opens an exact task or session "through Accessibility" and selects the terminal tab with
Apple Events ("exact terminal tab focus"). The rebuild: a followed app's conversation is opened
by raising the window with that title or pressing the sidebar entry with that title; a Claude
Code / Codex session's tab is found by the terminal device its hook reports (X-TTY) in Terminal or
iTerm2; other terminals are brought forward. Each listed conversation gets its own key.
