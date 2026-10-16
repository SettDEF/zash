# zash

A terminal emulator written entirely in Rust: correct, fast, smooth, themed by
tinct, and programmable. Part of the zett family, usable on its own.
See [PLAN.md](PLAN.md).

```sh
cargo run --release                      # your shell in a window
cargo run --release -- -e htop           # a program in a window
cargo run --release -- --snapshot out.png --size 100x30 -- ls --color=always
                                         # headless: print the screen, render a PNG
```

Keys: Ctrl+Shift+C / Ctrl+Shift+V copy and paste · Ctrl+= / Ctrl+- / Ctrl+0
font size · Shift+PageUp / Shift+PageDown scroll · select with the mouse
(also fills the middle-click selection).
