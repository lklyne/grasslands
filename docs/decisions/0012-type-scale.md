# 0012 Type scale

Date: 2026-10-16. Status: accepted.

The UI base size (the window's rem) is 14px, not GPUI Kit's 16, for desktop density. GPUI's `text_*`, spacing and rem-sized controls all scale from it, so the whole Kit UI shrinks together. `theme::apply_base` sets `Theme::font_size` after every theme load (a load resets it from the theme's tokens); the overlay window sets its rem from the same value.

The ramp at 14:

- `text_base` 14: nothing uses it on purpose; it's what unsized text inherits.
- `text_sm` 12.25: UI text. Panel titles, the tab strip, pane headers, the titlebar, file rows, menus and tooltips (the Kit's own `text_sm`).
- `text_xs` 10.5: captions. Count and status tags (pinned with `.text_xs()`, since a Kit tag's xsmall size is 0.625rem, 8.75px).
- `Theme::mono_font_size` 13: the herdr-style monospace rows in Spaces and Agents.

Fixed pixel values left in the app are layout, not type: row heights (file rows 24, mono lines 17, pane headers 22), the 31px tab strip that meets the dock's 30px title bar, gaps and positions.
