# Homebrew tap for Z Report

[Z Report](https://github.com/alikayhan/z-report-releases) turns your Claude Code and
Codex sessions into a private, evidence-backed journal of what you actually got done. This tap
is the quickest way to install it on an Apple Silicon Mac running macOS 13 or newer:

```sh
brew install --cask alikayhan/tap/z-report
```

Or tap once and install like any other cask:

```sh
brew tap alikayhan/tap
brew install --cask z-report
```

## What you get

The Cask installs the signed, notarized DMG from
[z-report-releases](https://github.com/alikayhan/z-report-releases) into `/Applications`,
so it launches with no Gatekeeper prompt. Z Report needs the
[Claude Code CLI](https://claude.com/product/claude-code) or the
[Codex CLI](https://github.com/openai/codex), signed in, to run evaluations: it uses
Claude Code when present and falls back to Codex. Any install of either works, including
`brew install --cask claude-code` or `brew install --cask codex`.

## Staying current

Z Report can update itself: it tells you when a release is out and installs it only when
you confirm, never mid-evaluation. Homebrew works too:

```sh
brew upgrade --cask z-report
```

## Uninstall

```sh
brew uninstall --cask z-report        # keeps your journal and settings
brew uninstall --zap --cask z-report  # also removes all data
```

Installation details, how updates work, and the full privacy statement live in the
[Z Report documentation](https://github.com/alikayhan/z-report-releases). Problems with
the Cask itself? [Open an issue here](https://github.com/alikayhan/homebrew-tap/issues).
