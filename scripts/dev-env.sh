# Source this: `. scripts/dev-env.sh`. Points every herdr call and the app at an
# isolated herdr 0.9.5 dev session, never the user's live default session.
# Each checkout gets its own session (dev-<dir name>) so parallel agents don't
# kill each other's panes; override with RT_SESSION. Folder names over 16
# characters are cut to 9 plus a hash of the checkout path, because herdr's
# sockets live under the session folder and macOS caps socket paths at 104 bytes.
unset HERDR_ENV HERDR_PANE_ID HERDR_TAB_ID HERDR_WORKSPACE_ID
_rt_root=$(git -C "$(dirname "${0:-.}")" rev-parse --show-toplevel 2>/dev/null || pwd)
export XDG_CONFIG_HOME="$HOME/.local/share/grasslands-herdr"
export HERDR_BIN="$HOME/.local/herdr-0.9.5/herdr"
_rt_dir=$(basename "$_rt_root")
if [ ${#_rt_dir} -gt 16 ]; then
  _rt_dir="$(printf %s "$_rt_dir" | cut -c1-9)-$(printf '%06x' "$(printf %s "$_rt_root" | cksum | cut -d' ' -f1)" | cut -c1-6)"
fi
export HERDR_SESSION="${RT_SESSION:-dev-$_rt_dir}"
export HERDR_SOCKET_PATH="$XDG_CONFIG_HOME/herdr/sessions/$HERDR_SESSION/herdr.sock"
export ZIG="${ZIG:-$HOME/.local/zig-0.16/zig}"
