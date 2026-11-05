# Desktop run notes

## Development

`cargo run` opens the app unbundled and defaults to the local Rails API at `http://localhost:3005`.
Its config and database live under `mtasks-rusty` (`dirs::config_dir()` / `dirs::data_local_dir()`,
e.g. `~/Library/Application Support/mtasks-rusty/jait.db`).

## Toolchain for the Terminal engine

The terminal engine links `libghostty-vt`, whose build script runs `zig build`. Zig comes from mise,
never Homebrew, and is pinned to exactly 0.17.1 in `mise.toml`.

- Run `mise install` once in each fresh worktree, then build through mise so Zig is on `PATH`:
  `mise exec -- cargo build` (or activate mise in your shell and run plain `cargo build`).
- The first build fetches Ghostty from GitHub, so it needs network access. To build offline, point
  `GHOSTTY_SOURCE_DIR` at a local Ghostty checkout of the pinned commit.
- The pins move together, on purpose: the `libghostty-vt` commit in `Cargo.toml`, Zig in `mise.toml`
  and `portable-pty` (`=0.9.0`). crates.io `libghostty-vt` 0.2.2 only builds on Zig 0.15, so the
  crate comes from git. Never bump one without the others (see `docs/adr/0002-*`).
- The built app has no Zig, Ghostty or Homebrew runtime dependency.

## Dev and release builds are separate

Debug or release decides both the server and the folder (`src/config.rs`):

| Build | Default API | Folder in `~/Library/Application Support` |
|---|---|---|
| `cargo run` | `http://localhost:3005` | `mtasks-rusty` |
| `--release` (including `cargo run --release`) | `https://justanotherissuetracker.com` | `JAIT` |

Never point one folder at both servers. Local rows are matched by `remote_id`, and saving Settings
starts a sync, so queued dev edits would push to prod and IDs would collide. The API token is never
built in, and it lives in that build's `config.toml`.

With no token, a release build opens Settings on launch and seeds nothing; its teams arrive with the
first sync. A dev build seeds a local "My Project" team instead, so it stays usable offline.

## Building and launching the `.app` bundle (macOS)

One-time: `cargo install cargo-bundle --locked`

Build: `cargo bundle --release`

Output: `target/release/bundle/osx/JAIT.app` (cargo-bundle also writes a `.dmg` beside it).
Launch: `open target/release/bundle/osx/JAIT.app`

- Bundle id: `com.jait.desktop`, set in `[package.metadata.bundle]` in `Cargo.toml`. macOS keys
  notification permission off it, so do not change it.
- The bundle is a release build, so it uses prod and the `JAIT` folder, not your dev data.
- This plain bundle is not signed or notarised. For a signed build, see below.
- Check the id: `/usr/libexec/PlistBuddy -c "Print CFBundleIdentifier" target/release/bundle/osx/JAIT.app/Contents/Info.plist`

## Signed release build (macOS)

`./scripts/build-release.sh` runs the tests, bundles, signs with the Developer ID, notarizes,
staples, and writes `JAIT.app` plus `JAIT-<version>.dmg` to `exports/<yymmdd>/jait-<version>`. A
second run the same day goes to `jait-<version>-2`, and so on. Old folders are never touched.

- The script stops if the binary contains `localhost:3005` or lacks the prod URL.
- Before a release, bump `version` in both `[package]` and `[package.metadata.bundle]` in
  `Cargo.toml`. The script stops if they differ.
- `--skip-tests` skips `cargo test`. `--no-notarize` signs without uploading to Apple, into a folder
  suffixed `-unnotarized`; other Macs reject that build.
- Credentials: the notarytool keychain profile defaults to `BrokenColony`; override it with
  `NOTARY_PROFILE=<name>`. `SIGN_IDENTITY=<name>` overrides the identity when the keychain has more
  than one Developer ID Application certificate.
- Only the DMG is submitted and stapled; notarizing it covers the app inside. Distribute the DMG, not
  the loose `JAIT.app`. Gatekeeper looks up the app's ticket online the first time it opens.
- Rejected? `xcrun notarytool log <submission-id> --keychain-profile BrokenColony` gives the reason.
- A failed run leaves a half-filled folder. It is harmless: the next run takes the next number.
- The build is arm64 only, so it won't run on Intel Macs.
