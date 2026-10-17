# Local patches to gpui-component 0.7.3

Vendored from crates.io and applied through `[patch.crates-io]`. See
`docs/decisions/0015-changes-tab.md`.

- `DiffState`'s list gets 1000px of overdraw instead of none. GPUI's list
  forgets the diff's uniform row-height hint whenever its width changes (its
  first layout included), so rows below the viewport count as 0px tall. With
  no overdraw, the scroll limit sat at the last painted row and the wheel
  barely moved the diff. Overdraw measures rows past the viewport, giving the
  wheel room; scrolling then measures further.

Only `src/diff/state.rs` changes. Drop this vendor copy once GPUI Kit ships
a fix.
