# Terminals use libghostty-vt pinned to a git-main commit with an exact Zig

Terminals run on `libghostty-vt` from GitHub main, pinned to commit `3d8befe806d9c0801919b7c0209d4e13be943fd1`, built with Zig exactly 0.17.1, with `portable-pty` 0.9.0 for the PTY. We chose this because a spike (JRST-328) ran a real shell in an Iced 0.13 pane and "felt ok" in hands-on use with `vim` and `claude`, with a plain flood at about 92 MB/s and 62 redraws/s, 0% idle CPU, and under 1 ms from key to published frame. The crates.io release (0.2.2) only builds on Zig 0.15 and fails on 0.17, while main builds on 0.17, so a git pin is the only version that works with the Zig we can install. `iced_term` was rejected because its only Iced 0.13 release (0.6.0) is frozen, and later releases target Iced 0.14. The evidence is in the comments on JRST-325, JRST-326 and JRST-328.

A git-main pin with an exact compiler version is surprising and hard to reverse, so the pins are upgraded together, on purpose: the `libghostty-vt` commit in `Cargo.toml`, Zig in `mise.toml`, and `portable-pty`.

## Considered Options

- **`libghostty-vt` 0.2.2 from crates.io.** It's the normal way to depend on a crate, but it says "Zig 0.15.x" and fails on 0.17. Choosing it would mean pinning an older Zig and never getting main's fixes.
- **`iced_term` 0.6.0.** It's a ready widget built on `alacritty_terminal`, with live theme and font commands. But it's frozen on Iced 0.13 (0.7.0 and 0.8.0 need Iced 0.14), takes only a program and arguments with no working directory or environment in that release, and has no way to pass app shortcuts (Cmd+K, Cmd+J, Cmd+1–9) through to the app. Nobody had built it against this app. It stayed as the fallback until the spike passed.
- **Our own emulator.** Not considered seriously. VT parsing, reflow, scrollback and key and mouse encoding are the part `libghostty-vt` already provides.

## Consequences

- The build needs Zig 0.17.1 through mise, and the first build fetches Ghostty from GitHub, so it needs network access unless `GHOSTTY_SOURCE_DIR` is set. Worktrees need Zig too.
- The `libghostty-vt` API is documented as unstable. Exactly one engine file, `src/terminal/engine.rs`, touches `libghostty-vt` and `portable-pty`, so a break stays in one place and no type from either crate appears in the Terminal API.
- Its types are `!Send` and `!Sync`, so each Terminal needs an owner thread plus a PTY reader and writer thread, and the UI sees only plain-data frames. Dropping a Terminal isn't enough to stop it, because the threads hold each other's channels. It must be closed explicitly (the spike leaked 150 threads, 50 shells and 150 file descriptors over 50 spawns).
- A crash in the Zig code takes down the app and every shell. A Rust panic on an owner thread marks only that Terminal as dead. We accept this risk.
- Drawing, fonts, selection and links are ours to build.
- Not yet verified: `cargo bundle` with the Zig build step, and `PATH` on a true Finder launch.
