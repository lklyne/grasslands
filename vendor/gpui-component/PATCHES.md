# Local patches to gpui-component 0.7.3

Vendored from crates.io and applied through `[patch.crates-io]`. See
`docs/decisions/0015-changes-tab.md`.

- `DiffState`'s list gets 1000px of overdraw instead of none. GPUI's list
  forgets the diff's uniform row-height hint whenever its width changes (its
  first layout included), so rows below the viewport count as 0px tall. With
  no overdraw, the scroll limit sat at the last painted row and the wheel
  barely moved the diff. Overdraw measures rows past the viewport, giving the
  wheel room; scrolling then measures further. (`src/diff/state.rs`)
- The tab bar's `…` menu adds Zoom In/Out only when the panel's
  `zoom_control` puts it in the menu, instead of always adding it disabled.
  (`src/dock/tab_panel.rs`)
- `WindowExt::host_tooltips_in` and `own_tooltip_overlay` let one window's
  tooltips show in another's tooltip overlay. The app sends the main
  window's to the overlay window, since the native terminal views cover
  anything drawn in the main window. (`src/root.rs`, `src/window_ext.rs`)

Drop this vendor copy once GPUI Kit ships these.
