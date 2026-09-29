cask "z-report" do
  version "0.2.4"
  sha256 "052ad607eb193cb116cecc3cf66b2fd764018a7b92300f95f868866ba151dd09"

  url "https://github.com/alikayhan/z-report-releases/releases/download/v#{version}/Z-Report_#{version}_aarch64.dmg"
  name "Z Report"
  desc "Accomplishment journal for Claude Code and Codex sessions"
  homepage "https://github.com/alikayhan/z-report-releases"

  auto_updates true
  depends_on arch: :arm64
  depends_on macos: :ventura

  app "Z Report.app"

  uninstall quit: "com.alikayhan.zreport"

  zap trash: [
    "~/Library/Application Support/com.alikayhan.zreport",
    "~/Library/Caches/com.alikayhan.zreport",
    "~/Library/HTTPStorages/com.alikayhan.zreport",
    "~/Library/Preferences/com.alikayhan.zreport.plist",
    "~/Library/Saved Application State/com.alikayhan.zreport.savedstate",
    "~/Library/WebKit/com.alikayhan.zreport",
  ]

  caveats <<~EOS
    Z Report needs the Claude Code CLI or the Codex CLI, signed in, to run
    evaluations. It uses Claude Code when present and falls back to Codex.
    Any install of either works; with Homebrew:

      brew install --cask claude-code
      brew install --cask codex
  EOS
end
