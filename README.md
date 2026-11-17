# switcheroo
very much wip but kinda works

## usage + building
Accessibility, Device Control and Data Access, and Screen/System Audio Recording need to be enabled to function correctly.

Cmd + D opens the switcher, ↑/↓ or Ctrl-p/Ctrl-n to move, Enter to switch/focus window, Esc to close.

You can use [`cargo-bundle`](https://github.com/burtonageo/cargo-bundle) to build a .dmg:
```bash
cargo install cargo-bundle
cargo bundle --release
```
or just run it in a terminal
```bash
cargo run --release
```
or just download the CI artifact.

## why
1. I honestly just need a window switcher on macOS (like [rofi](https://github.com/davatorium/rofi)) where the mouse follows focus (which the Raycast builtin switcher cannot afaict), and I don't like cycling through windows constantly with [Alt-Tab](https://github.com/lwouis/alt-tab-macos).
2. procrastination

## acknowledgements
- [AltTab](https://github.com/lwouis/alt-tab-macos) and [yabai](https://github.com/asmvik/yabai) for documenting private macOS apis, as well as inspiration for a lot of the core logic
- The space switching logic was ported over from [InstantSpaceSwitcher](https://github.com/jurplel/InstantSpaceSwitcher)
- [Apple](https://en.wikipedia.org/wiki/Criticism_of_Apple_Inc.) for making my life very difficult
