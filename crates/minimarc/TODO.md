# minimarc cleanup — from the 2026-11-19 review

Status: `[ ]` not started · `[~]` in progress · `[x]` done

## Bugs and leftovers

1. [x] Two key paths: bar/swipe key releases go through `startup_keys`
   (straight to the core) while presses go through `key_queue`; a backed-up
   queue lets the release beat the press and the key sticks. Release through
   `press()` (`MIN_KEY_FRAMES` already makes a short tap land) and feed
   startup keys through `press()` too; drop `KEY_HOLD`.
2. [x] Drop `#![allow(dead_code)]` in `player.rs` and remove what it hid
   (`fragment_none`/`fragment_full`, `load_core`'s `Option`; `from_keycode` is Android-only). `Joy` stays as a placeholder.
3. [x] Remove the `KEYDBG` info logs.
4. [x] Identical branches in `touch()` on lift. A tap toggling the key bar is
   right; fix `CLAUDE.md` ("a tap on a running demo opens it").
5. [x] Misplaced doc comments: `fit_rect`'s sits on `KEYBOARD_MARGIN`,
   `SWIPE_DIST`'s describes `SWIPE_TIME`.

## Simplifications

6. [x] One `unload()` for the teardown `start()` and `stop()` share, clearing
   the key state too.
7. [x] Touch state (`touches`, `swipe`, `bar_key`, `bar_press`, `nav_swipe`,
   `warp`, `ui.key_held`) as one `Gesture` enum; `warp` and `key_held` derived.
8. [x] One render pass for the CRT and egui.
9. [x] `Ui`: one `hud_text(loc, text, time)` helper and a `FOREVER` const.
10. [x] `Ui::bar_visible()` for the three `keys_open && !picker.is_open()`.
11. [x] Key bar rows as `ROWS: [&[Button]; 2]`; `closes_bar` from that;
    `bar_key` as `Button::key()` in `ui.rs`.
12. [x] Small repeats: `App::load(file)`, `toggle_pause()`, `draw()` returning
    `Option<EmuFile>` instead of a one-variant `Action`, the `match` in
    `ui_event`, `alt_key` digits as `RETROK_0 + i`, shared asset install for
    Android and iOS.

## Generalization

13. [ ] Split `player.rs`: `keys.rs` (table, alt keys, key state), `gfx.rs`,
    `platform.rs` (assets, `android_main`, iOS). Update "Where things are" in
    `CLAUDE.md`.
14. [ ] Optional: share a name → `retro_key` table in `retro-core` between
    `key_by_name` and demarc's `Emulator::resolve_key_name`.
