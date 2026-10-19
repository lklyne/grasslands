# Template for github.com/lklyne/homebrew-tap (Casks/grasslands.rb).
# Install: brew install --cask lklyne/tap/grasslands
#
# version and sha256 track the latest release; the tap holds the live copy
# (see README.md next to this file).
cask "grasslands" do
  version "0.1.1"
  sha256 "af490ae45cabb4df4e692d76b2680d7f2a465a91899507472148afd7a237542d"

  url "https://github.com/lklyne/grasslands/releases/download/v#{version}/grasslands-#{version}-arm64.zip"
  name "grasslands"
  desc "Native window for the herdr agent multiplexer"
  homepage "https://grasslands.dev/"

  livecheck do
    url :url
    strategy :github_releases
  end

  # Sparkle updates the app in place (docs/decisions/0024-updates.md), so
  # `brew upgrade` leaves it alone unless asked with --greedy.
  auto_updates true
  depends_on arch: :arm64
  depends_on macos: :ventura

  app "grasslands.app"

  zap trash: [
    "~/.config/grasslands",
    "~/Library/Application Support/grasslands",
    "~/Library/Logs/grasslands",
    "~/Library/Saved Application State/dev.lklyne.grasslands.savedState",
  ]

  caveats <<~EOS
    grasslands runs on herdr 0.9.5 or newer. Open grasslands to finish
    setup: it uses the herdr on your PATH, and offers to install or update
    it (through Homebrew, mise or herdr's installer) when it's missing or
    old. It attaches to herdr's terminals with --takeover, so a herdr TUI
    attached to the same panes loses them.
  EOS
end
