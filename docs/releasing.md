# Releasing

The version lives in `src-tauri/Cargo.toml`, `crates/z-report-core/Cargo.toml`,
`crates/z-report-cli/Cargo.toml`, and `mods/claude-code/.claude-plugin/plugin.json`;
packaging fails unless all four match, and the release workflow fails unless the pushed
tag matches `src-tauri/Cargo.toml`. `package.json` and `tauri.conf.json` deliberately
carry none. Bump the version, run `cargo check` so `Cargo.lock` follows, then tag:

```sh
git tag -a v0.2.0 -m "What changed, published as the release notes"
git push origin v0.2.0
```

The tag triggers `.github/workflows/release.yml`, which tests, builds
`aarch64-apple-darwin`, signs, notarizes, and staples the app and DMG, and publishes the
DMG, updater archive (`.app.tar.gz` + `.sig`), the notarized Claude Code mod archive,
`latest.json`, and `checksums.txt` as a GitHub release on this repository, then commits
`.claude-plugin/marketplace.json` on `main` pointing at the new mod archive. Installed
apps discover the release through `latest.json`, and the mod through the marketplace. Homebrew users get it once `Casks/z-report.rb` in
[homebrew-tap](https://github.com/alikayhan/homebrew-tap) is bumped; the workflow's run
summary includes a paste-ready Cask rendered from `packaging/homebrew/Casks/z-report.rb` with the new
version and DMG SHA-256 filled in.

Required GitHub Actions secrets:

| Secret | Contents |
| --- | --- |
| `APPLE_CERTIFICATE` | Developer ID Application certificate, base64-encoded `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | Password of the `.p12` |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Name (TEAMID)` |
| `APPLE_API_ISSUER` | App Store Connect API issuer ID |
| `APPLE_API_KEY` | App Store Connect API key ID |
| `APPLE_API_KEY_CONTENT` | Contents of the App Store Connect `AuthKey_*.p8` file |
| `TAURI_SIGNING_PRIVATE_KEY` | Contents of `~/.tauri/z-report.key` |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Its password (empty if none) |

The updater private key exists only in `~/.tauri/z-report.key` and the CI secret. Back it
up somewhere durable: shipped apps embed the public key and will reject updates signed by
any other key, so losing it strands every installed copy on its current version.
