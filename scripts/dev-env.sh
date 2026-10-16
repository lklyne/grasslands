# Source this: `. scripts/dev-env.sh`. Points every herdr call and the app at the
# isolated herdr 0.9.5 dev session, never the user's live default session.
unset HERDR_ENV HERDR_PANE_ID HERDR_TAB_ID HERDR_WORKSPACE_ID
export XDG_CONFIG_HOME="$HOME/.local/share/rusted-tools-herdr"
export HERDR_BIN="$HOME/.local/herdr-0.9.5/herdr"
export HERDR_SESSION=gpui-dev
export HERDR_SOCKET_PATH="$XDG_CONFIG_HOME/herdr/sessions/gpui-dev/herdr.sock"
export ZIG="$HOME/.local/zig-0.16/zig"
