# lazymenu-cli

`lazymenu-cli` is a native, config-driven command palette. It reads `menu.toml` from the current directory first, then falls back to the system menu at `/etc/lazymenu-cli/menu.toml`.

## Build and install

```bash
cargo build --release
./target/release/lazymenu-cli
```

Install it on your path with:

```bash
cargo install --path .
lazymenu-cli
```

Use another config with `--config /path/to/menu.toml`.

`cargo install --path .` installs only the executable, so use `--config` or keep a project-local `menu.toml` when installing from source.

The Arch Linux package also installs the system fallback. Download it from a release and install it with:

```bash
sudo pacman -U ./lazymenu-cli-<version>-x86_64.pkg.tar.zst
```

## Keyboard controls

In browse mode:

- `↑`/`↓` or `j`/`k` moves one item.
- `PageUp`/`PageDown` moves one visible page.
- `Home`/`End` jumps to the first or last result.
- `/` opens search.
- `Enter` runs the selected command.
- Configured one-character bindings run items directly.
- The configured quit key (default `q`), `Esc`, or `Ctrl+C` exits.

In search mode, type a query and use the same navigation keys. Search is case-insensitive and fuzzy across labels, groups, tags, descriptions, commands, and direct bindings. `Esc` clears a non-empty query; with an empty query it returns to browse mode. Press `Esc` again to quit.

The interactive menu uses the terminal's alternate screen. Quitting restores the terminal contents that were visible before the menu opened instead of leaving the menu drawn on screen or clearing scrollback.

Favorites appear first, followed by recently used commands and then remaining commands ordered by group. `★` marks a favorite and `↻` marks a recent command.

## Configuration

[`menu.toml`](menu.toml) is the system fallback installed at `/etc/lazymenu-cli/menu.toml`. A `menu.toml` in the current project directory takes priority. Each item needs `label` and `command`; all other fields are optional.

```toml
[menu]
title = "Project tools"
quit_key = "q"
loop = true
key_format = "{key})"
selected_foreground = "black"
selected_background = "cyan"
selected_bold = true

[[items]]
id = "run-tests"
label = "Run test suite"
key = "t"
command = ["cargo", "test"]
group = "Quality"
tags = ["tests", "check", "ci"]
description = "Run all Rust unit and integration tests"
favorite = true
confirm = false

[[items]]
id = "clean-build"
label = "Clean build output"
command = "cargo clean && rm -f coverage.info"
group = "Maintenance"
tags = ["clean", "build"]
description = "Remove generated build and coverage files"
confirm = true
```

Array commands execute a program directly. String commands run through the platform shell, allowing pipes, redirects, and shell variables. Treat configs as executable code and only use files you trust.

`loop = true` returns to the menu after each command and is the default when the option is omitted. Set `loop = false` to exit immediately after the selected command finishes. In non-looping mode, lazymenu-cli exits with the command's status and does not show the return-to-menu prompt.

An explicit `id` keeps recent-command tracking stable when labels or commands change. IDs may contain letters, numbers, `.`, `_`, and `-`. Without an ID, lazymenu-cli derives a stable hash from the label and command.

Bindings are case-insensitive, exactly one character, and unique. The quit key, `j`, `k`, and `/` are reserved. Arrow keys, paging keys, Home, End, Enter, and Escape are built-in controls.

`key_format` controls how bindings appear and must contain `{key}` exactly once. For example, use `"[{key}]"`, `"{key})"`, or `"{key}:"`. The key column remains aligned even when some items do not have bindings.

The selected row is a full-width color bar. `selected_foreground` and `selected_background` accept standard terminal color names such as `black`, `cyan`, `dark_blue`, and `white`, or a `#RRGGBB` value. Set `selected_bold = false` if you do not want bold selected text. These styles are disabled when `NO_COLOR` is set.

Configs are limited to 1 MiB and 1,000 items.

## Recent commands and XDG state

The 20 most recently used commands are stored as stable IDs at:

```text
$XDG_STATE_HOME/lazymenu-cli/recent-items
```

If `XDG_STATE_HOME` is unset or not absolute, the XDG default is used:

```text
$HOME/.local/state/lazymenu-cli/recent-items
```

State write failures never prevent commands from running; the menu reports the failure in its status line.

## Script-friendly modes

```bash
lazymenu-cli --print
lazymenu-cli --dry-run
```

Run validation with:

```bash
cargo test
./tests/test_cli.sh
```
