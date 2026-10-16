# Local patches to gpui-libghostty 0.3.1

Vendored from crates.io and applied through `[patch.crates-io]`. See
`docs/decisions/0001-ghostty-gpui-kit.md`.

- `TerminalConfiguration::File(PathBuf)`: a surface starts from Ghostty's
  built-in defaults plus one config file chosen by the application, so each
  terminal can have its own font, theme and (later) shader.
- `Terminal::load_config_file(&Path)`: reconfigures a running terminal from a
  new config file without restarting its process.
- `Terminal::update_theme` on a `File` surface layers the new colors over a
  copy of that file instead of dropping its settings.

Only `src/terminal.rs` and `src/adapter.rs` change; `vendor/ghostty` and the
shim are untouched, so the cached native Ghostty build is shared with the
unpatched crate.
