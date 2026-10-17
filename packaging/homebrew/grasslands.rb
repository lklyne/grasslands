# Draft cask for github.com/lklyne/homebrew-tap (Casks/grasslands.rb).
# Install: brew install --cask lklyne/tap/grasslands
#
# PLACEHOLDER: version and sha256 are not real. Set them from the release
# asset when cutting a release (see README.md next to this file).
cask "grasslands" do
  version "0.1.0" # PLACEHOLDER
  sha256 "0000000000000000000000000000000000000000000000000000000000000000" # PLACEHOLDER

  url "https://github.com/lklyne/grasslands/releases/download/v#{version}/grasslands-#{version}-arm64.zip"
  name "grasslands"
  desc "Native macOS window for the herdr agent multiplexer"
  homepage "https://grasslands.dev"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on arch: :arm64
  depends_on macos: ">= :ventura"
  depends_on formula: "herdr"

  app "grasslands.app"

  # Only while releases are unsigned; delete once they're notarized.
  # See README.md, "Signing".
  postflight do
    system_command "/usr/bin/xattr",
                   args: ["-dr", "com.apple.quarantine", "#{appdir}/grasslands.app"]
  end

  caveats <<~EOS
    grasslands needs herdr 0.9.5. It attaches to herdr's terminals with
    --takeover, so a herdr TUI attached to the same panes loses them.
  EOS

  zap trash: [
    "~/.config/grasslands",
    "~/Library/Application Support/grasslands",
    "~/Library/Logs/grasslands",
    "~/Library/Saved Application State/dev.lklyne.grasslands.savedState",
  ]
end
