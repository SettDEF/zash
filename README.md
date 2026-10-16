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

Keys: Ctrl+Shift+F search the scrollback · Ctrl+click open a link ·
Ctrl+Shift+C / Ctrl+Shift+V copy and paste · Ctrl+Shift+Up / Down jump between
prompts · Ctrl+Shift+O copy the last command's output · Ctrl+= / Ctrl+- / Ctrl+0
font size · Shift+PageUp / Shift+PageDown scroll · select with the mouse
(also fills the middle-click selection).

## Shell integration

zsh needs nothing: zash starts it with a tiny .zshenv of its own
(`~/.local/share/zash/shell/zsh/.zshenv`) that restores your ZDOTDIR, sources
your .zshenv and adds two hooks. Your files are not edited. Each prompt then
gets a bar at the left edge (red after a failed command), and you can jump
between prompts and copy a command's output. `ZASH_NO_SHELL_INTEGRATION=1`
turns it off.

## Settings

`~/.config/zash/zash.toml` (written on first start, with comments): font, size,
line height, font features, padding, scrollback, cursor animation, opacity
(follows the shell's Glass panel unless set), colour file (follows tinct).
