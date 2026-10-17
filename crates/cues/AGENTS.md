# cues

`cues` is fourteen interaction sounds for grasslands, one for each interface
job, synthesized at play time. It is a Rust port of
[cuelume](https://github.com/danielwh2/cuelume) (MIT) at commit `2a1ba4f`,
whose agent guide is at https://cuelume.dev/agents.md. Same cues, same
recipes, same themes; no audio files.

This page is the complete guide for AI agents adding sound to grasslands.

## Quickstart

The app holds one shared `Cues`, reached through `crate::sounds::cues()`
(`None` only if the worker couldn't start). Play from any thread; playing
never blocks and never fails.

```rust
use cues::{Cue, PlayOptions};

if let Some(cues) = crate::sounds::cues() {
    cues.play(Cue::Tap, PlayOptions::default());
}
```

The theme, volume and on/off come from Settings → Sounds
(`crates/app/src/sounds.rs`); don't set them from feature code.

`Cues::new()` opens no device: the output stream starts on the first play and
pauses when everything has rung out. With no output device every play is a
silent no-op.

## Two ways to use it

### 1. From event handlers, for interface chrome

GPUI has no declarative bindings, so play from the handler. Map them the way
cuelume's attributes do:

| GPUI event                                   | cuelume attribute       | Cue        |
| -------------------------------------------- | ----------------------- | ---------- |
| `on_click` on a button or link               | `data-cuelume-tap`      | `Tap`      |
| a key that edits text                        | `data-cuelume-type`     | `Type`     |
| picking an option in a dropdown, list, tabs  | `data-cuelume-select`   | `Select`   |
| a checkbox or switch flipping                | `data-cuelume-toggle`   | `Toggle`   |
| a menu, panel or dialog opening              | `data-cuelume-open`     | `Open`     |
| the same closing                             | `data-cuelume-close`    | `Close`    |
| moving between workspaces, tabs or pages     | `data-cuelume-navigate` | `Navigate` |

One cue per action: when nested elements both handle it, the innermost plays
and stops propagation.

A control sounds as it goes down and lets go as it comes up: call `press` on
mouse down and `release` on the click that follows. Keyboard activation plays
both at once with `play`. Only a theme with a release makes the let-go
audible; `Press` is the first.

### 2. From code, for outcomes and progress

```rust
use cues::{Cue, Direction, Emphasis, PlayOptions};

cues.play(Cue::Loading, PlayOptions::default());
match deploy() {
    Ok(report) if report.warnings.is_empty() => cues.play(Cue::Success, PlayOptions::default().emphasis(Emphasis::Strong)),
    Ok(_) => cues.play(Cue::Warning, PlayOptions::default()),
    Err(_) => cues.play(Cue::Error, PlayOptions::default()),
}

cues.play(Cue::Navigate, PlayOptions::default().direction(Direction::Back));
cues.play(Cue::Count, PlayOptions::default().duration(Duration::from_millis(900)));
```

## Cues

| Cue         | Job                                   | Character                                    |
| ----------- | ------------------------------------- | -------------------------------------------- |
| `Tap`       | Buttons, links, direct activation     | Small glassy tap                             |
| `Type`      | Text entry                            | Keyboard keystroke, different every stroke   |
| `Select`    | Dropdowns, menus, lists               | Crisp woody detent                           |
| `Toggle`    | Switching between states              | One crisp snap with a knock of body          |
| `Open`      | Menus, drawers, dialogs, disclosures  | Air drawing up over a light mallet note      |
| `Close`     | Closing or dismissing                 | Air falling shut over a low, damped note     |
| `Navigate`  | Routes, pages, carousels, galleries   | Soft whoosh that rises, and falls going back |
| `Success`   | Confirmed completion                  | One soft mallet chord, C–G–E spread wide     |
| `Warning`   | Done, but needs a look                | One mallet fifth, A and E                    |
| `Error`     | Recoverable failure or refusal        | One muted low chord, short                   |
| `Loading`   | Slow work started                     | One soft, low note that swells and fades     |
| `Ready`     | A result is there, nothing confirmed  | One warm glass note in a small room          |
| `Attention` | Blocked until the user answers        | One high glass bell, ringing longest         |
| `Count`     | A number animating to a new value     | One breath that rises with the count         |

The material tells you the kind of event before you know which one: mallets for outcomes, glass for presence, air for motion, wood and keys for input.

## Which cue?

Pick by the job, not by the sound. Every row uses a cue that exists; emphasis and options do the rest.

| Moment | Cue |
| --- | --- |
| Agent blocked on a permission prompt or question | `Attention` |
| Agent finished while you were elsewhere | `Ready` |
| Agent finished in the tab you're looking at | nothing, or `Ready`, subtle |
| Agent exited with an error | `Error` |
| Agent started working | nothing, or `Loading`, subtle |
| Pane or tab closed | `Close`, subtle |
| Workspace or tab switch | `Navigate`, subtle, `direction` |
| Reorder a sidebar item | `Select`, `direction` |
| Drag picked up / dropped / cancelled | `Select` / `Tap`, strong / `Close`, subtle |
| Primary button: save, send, submit | `Tap` |
| Secondary or ghost button | `Tap`, subtle |
| Destructive confirm: delete, remove | `Close`, strong |
| Copy to clipboard | `Success`, subtle |
| Like, star, bookmark | `Toggle` |
| Undo / redo | `Navigate`, subtle, `Direction::Back` / `Forward` |
| Keystroke, delete, space, return | `Type`, with `key` |
| Autocomplete accepted | `Select` |
| Field fails validation | `Error`, subtle |
| Menu, dropdown, or list option | `Select` |
| Tab, segmented control, radio | `Select`, `direction` |
| Checkbox, switch | `Toggle`, `direction` |
| Slider or stepper step | `Select`, subtle, `direction` |
| Menu or popover opens / closes | `Open` / `Close`, subtle |
| Dialog or drawer opens / closes | `Open` / `Close` |
| Accordion expands / collapses | `Open` / `Close`, subtle |
| Command palette | `Open`, subtle, or nothing: it fires hundreds of times a day |
| Toast | the cue for what it reports, subtle |
| Saved, synced | `Success`, subtle |
| Publish, deploy confirmed | `Success`, strong |
| Partial failure, deprecation, connection retrying | `Warning` |
| Permission denied | `Error` |
| Upload, export, or build started | `Loading` |
| Export ready | `Ready` |
| Long job finished while the user was away | `Ready`, strong |
| New message in an open conversation | `Ready`, subtle |
| Reminder or timer due | `Attention` |
| A number animates to a new value | `Count`, `duration` matching the animation |
| A number counts down | `Count`, `Direction::Back` |
| A live number that updates constantly | nothing |
| Suggestion accepted / rejected | `Select` / `Close`, subtle |
| Switch model or mode | `Select` |
| One playful moment in a professional app | the usual cue, `theme(Theme::Bubble)` |

Never play a cue per streamed token, per tool call inside an agent run, per
terminal output line, or on hover. Those fire in bursts and wear any sound
out; play one cue when the run ends. An animated number gets one `Count` when
it starts moving, never one per digit.

## API (complete)

```rust
use cues::{Cues, Cue, Emphasis, Theme, Direction, KeyRole, Input, Context, PlayOptions};
```

- `Cues::new() -> Result<Cues, Error>`: the output. Cheap to clone, `Send + Sync`.
- `play(cue, options)`: play a cue now. Options, for this play only:
  `volume` (0–1), `emphasis` (`Subtle | Normal | Strong`), `direction`
  (`Forward | Back`, shapes `Select`; `Back` plays `Navigate`, `Toggle` and
  `Count` backwards), `key` (`Printable | Space | Delete | Enter`, shapes
  `Type`), `input` (`Mouse | Touch | Pen | Keyboard`, shapes `Tap`), `theme`
  (for this play only), `duration` (for `Count`, clamped to 300–2000 ms).
  A cue with a release plays it 90 ms after the press.
- `press(cue, options)` and `release(cue)`: the two halves of `play`, for a
  control that goes down and comes up. Call `press` as the gesture goes down
  and `release` with the same cue as it lifts. The release keeps its press's
  theme and emphasis. A second `release`, or one with no press waiting, plays
  nothing. A cue holds one press at a time.
- `press_gesture(cue, gesture, options)` and `release_gesture(cue, gesture)`:
  the same, held under `gesture` (such as a key's code), so presses that
  overlap each let go of their own.
- `set_enabled(bool)`: turn sound on or off. Off is immediate: cues already
  ringing fade within a few milliseconds. Doesn't persist; the app's settings
  own it.
- `set_volume(f32)`: global volume for future playback, clamped to `0–1`.
- `set_theme(Theme)`: `Default` (glass, wood, air, soft mallets), `Mech` (dry
  machined parts), `Bubble` (knocks, drips, corks, gulps; playful), or `Press`
  (a click over a deep note that rings, and a lighter click letting go; quick
  taps walk the note up three steps). Same cues, same levels in every theme.
- `cues::render(&Request, rate) -> Stereo`: one play as samples, with no
  device. For tests and offline rendering.

## Guarantees you can rely on

- A cue plays one voice at a time: playing it again fades the last play
  (80 ms, or 400 ms for note cues) instead of stacking on it. Other cues ring on.
- Quick repeats of `Type`, `Select`, `Tap`, `Toggle` and `Navigate` play
  lighter and shorter, so a burst never turns into a wall of clicks.
- Every play differs a little in pitch and weight, like a real object struck
  twice.
- Playing never blocks the caller and never panics. No device, a device that
  disappears, or sound turned off make `play` a silent no-op; the stream
  rebuilds itself when the output device changes.
- Nothing is stored or sent.

## Guidance for good sound design

- Pick the cue by its job; the tables above are the contract.
- Use emphasis for weight: `Subtle` for frequent actions, `Strong` for rare
  ones that matter.
- Sound is a setting (Settings → Sounds: theme, volume, off). Respect it;
  never play around it.
- Agents run for hours in grasslands: keep `Bubble` for single playful
  moments; `Default`, `Mech` and `Press` are for all-day use.
- Sounds come from status changes, not polling: play on the transition, once.
