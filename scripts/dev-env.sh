# Source this: `. scripts/dev-env.sh`. Points every herdr call and the app at an
# isolated herdr 0.9.5 dev session, never the user's live default session.
# Each checkout gets its own session (dev-<dir name>) so parallel agents don't
# kill each other's panes; override with RT_SESSION.
unset HERDR_ENV HERDR_PANE_ID HERDR_TAB_ID HERDR_WORKSPACE_ID
_rt_root=$(git -C "$(dirname "${0:-.}")" rev-parse --show-toplevel 2>/dev/null || pwd)
export XDG_CONFIG_HOME="$HOME/.local/share/grasslands-herdr"
export HERDR_BIN="$HOME/.local/herdr-0.9.5/herdr"
export HERDR_SESSION="${RT_SESSION:-dev-$(basename "$_rt_root")}"
export HERDR_SOCKET_PATH="$XDG_CONFIG_HOME/herdr/sessions/$HERDR_SESSION/herdr.sock"
export ZIG="$HOME/.local/zig-0.16/zig"
