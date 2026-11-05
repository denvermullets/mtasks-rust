# PROTOTYPE: terminal pane spike (JRST-328)

Throwaway. A real shell in one docked Iced 0.13 pane using `libghostty-vt` (main, pinned to rev 3d8befe) and `portable-pty` 0.9.0.
Never merged into the app. Needs Zig 0.17 to build (`mise.toml` here pins 0.17.1).

    cd spikes/terminal-pane && cargo run --release

Toolbar: theme, font, size, panel height, Option=Alt, Heavy app view, keys target.
Keys: Cmd+J toggles keys between terminal and app, Cmd+V pastes, Cmd+=/- change font size, Cmd+Q quits.
The bottom strip shows grid, cell size, throughput and redraw rate, refreshed each second.
