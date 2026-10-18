# Template for github.com/lklyne/homebrew-tap (Casks/grasslands.rb).
# Install: brew install --cask lklyne/tap/grasslands
#
# version and sha256 track the latest release; the tap holds the live copy
# (see README.md next to this file).
cask "grasslands" do
  version "0.1.0"
  sha256 "7fa6caa80d7cd4ddd8e3cd29e32bd6c6a466abaf23ba62d0c7ecfc9d7c642537"

  url "https://github.com/lklyne/grasslands/releases/download/v#{version}/grasslands-#{version}-arm64.zip"
  name "grasslands"
  desc "Native window for the herdr agent multiplexer"
  homepage "https://grasslands.dev/"

  livecheck do
    url :url
    strategy :github_releases
  end

  depends_on arch: :arm64
  depends_on formula: "herdr"
  depends_on macos: :ventura

  app "grasslands.app"

  zap trash: [
    "~/.config/grasslands",
    "~/Library/Application Support/grasslands",
    "~/Library/Logs/grasslands",
    "~/Library/Saved Application State/dev.lklyne.grasslands.savedState",
  ]

  caveats <<~EOS
    grasslands needs herdr 0.9.5. It attaches to herdr's terminals with
    --takeover, so a herdr TUI attached to the same panes loses them.
  EOS
end
