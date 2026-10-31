# Desktop run notes

## Development

`cargo run` opens the app unbundled. Config and database live in the same place as the bundle
(`dirs::config_dir()` / `dirs::data_local_dir()` under `mtasks-rusty`, e.g.
`~/Library/Application Support/mtasks-rusty/jait.db`).

## Building and launching the `.app` bundle (macOS)

One-time: `cargo install cargo-bundle --locked`

Build: `cargo bundle --release`

Output: `target/release/bundle/osx/JAIT.app` (cargo-bundle also writes a `.dmg` beside it).
Launch: `open target/release/bundle/osx/JAIT.app`

- Bundle id: `com.jait.desktop`, set in `[package.metadata.bundle]` in `Cargo.toml`. macOS keys
  notification permission off it, so do not change it.
- The bundle runs the same binary: same config, same database, same sync as `cargo run`.
- Not signed or notarised. If Gatekeeper complains on a copied build, right-click > Open.
- Check the id: `/usr/libexec/PlistBuddy -c "Print CFBundleIdentifier" target/release/bundle/osx/JAIT.app/Contents/Info.plist`
