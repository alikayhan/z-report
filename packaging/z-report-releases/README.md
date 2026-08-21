# Z Report

A local accomplishment journal for your Claude Code sessions. Z Report watches the
session transcripts Claude Code already keeps on your Mac, evaluates each day's work on
your own Claude Code account, and turns it into a reviewable journal of achievements you
can export for standups, weekly updates, or performance reviews.

This repository hosts release binaries and update metadata. Z Report runs on Apple
Silicon Macs (macOS 13 or newer) and requires the
[Claude Code CLI](https://claude.com/product/claude-code) — any install works (native
installer, npm, or Homebrew).

## Install

```sh
brew install --cask alikayhan/tap/z-report
```

Or download the DMG from the [latest release](https://github.com/alikayhan/z-report-releases/releases/latest)
and drag Z Report to Applications. Every release is signed, notarized, and checksummed
(see `checksums.txt` on the release).

## Updates

Z Report checks this repository for a new release about once a day, and installs an
update only when you confirm it — never while an evaluation is running. You can also
update through Homebrew:

```sh
brew upgrade --cask z-report
```

## Uninstall

```sh
brew uninstall --cask z-report        # keeps your journal and settings
brew uninstall --zap --cask z-report  # also removes all data
```

Without Homebrew: quit Z Report from the menu bar, delete it from Applications, and
remove `~/Library/Application Support/com.alikayhan.zreport` if you also want your data gone.

## Privacy and network boundary

- All product data (evidence, candidates, journal, settings) lives in
  `~/Library/Application Support/com.alikayhan.zreport/` — SQLite, no accounts, no sync.
- Z Report has **no backend, no analytics, and no telemetry**.
- Two things leave your Mac, and nothing else:
  1. Each evaluation runs `claude -p` on **your own Claude Code account**, sending the
     prepared evidence package (session excerpts, file paths, command results including
     those from delegated sub-sessions, the names of external tools used to change
     something, commit and pull request metadata) to Anthropic — the same boundary as
     using Claude Code itself. This is disclosed in Settings. Arguments passed to
     external tools are never included, only the server and tool name.
  2. The updater asks GitHub for the latest release metadata about once a day.
     The request carries nothing about you or your work, and updates only install with
     your confirmation — never while an evaluation is running.
