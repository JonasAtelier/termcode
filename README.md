# termcode

A VS Code-style code editor and Git panel that runs **inside your terminal**.

- Run `termcode` in a folder and it opens right there, like `vim`. Quit and you are back at your shell.
- Inside [herdr](https://herdr.dev), `termcode --tab` opens it as a **Code** tab instead.

What you get: a file Explorer, Search, Source Control with a commit graph, tabs and split
editors, side-by-side diffs, a minimap, Go to Definition, and VS Code themes.

### How light is it?

Both opening this repo and one file, fresh profiles, median of 5 runs (Linux, 12 cores):

| | termcode | VS Code 1.146 |
|---|---|---|
| Startup | **0.1 s** | 1.0 s |
| Memory (PSS) | **20 MB** | 800–1070 MB |
| Processes | 1 | 11 |
| CPU used in the first 20 s | 0.6 s | 12.4 s |
| CPU when idle | 0.9% | 9% |
| Install size | 8 MB | 1.9 GB |

termcode's numbers leave out the terminal it runs in. VS Code had no extensions and its
startup is its own timer, which leaves out the `code` launcher, so real use is heavier.

---

## 1. What you need

| Tool | Why | Check it |
|---|---|---|
| Linux | the only supported OS | |
| Rust 1.88 or newer | to build termcode | `cargo --version` |
| A C linker (`cc`) | Rust needs it to link | `cc --version` |
| git | Source Control | `git --version` |
| ripgrep (`rg`) | Search and Go to Definition | `rg --version` |
| herdr 0.9.1 or newer | only for the Code tab (`--tab`) | `herdr --version` |
| A Nerd Font | the icons | see step 2 |

Optional: [Ghostty](https://ghostty.org) (the terminal termcode is tested in), and
`wl-copy` or `xclip` for the clipboard.

### Install them

```sh
# git, ripgrep and a C linker: pick your distro
sudo apt install git ripgrep build-essential     # Debian / Ubuntu
sudo dnf install git ripgrep gcc                 # Fedora
sudo pacman -S git ripgrep base-devel            # Arch

# Rust (press Enter for the default install, then open a new terminal)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# herdr (optional, for the Code tab)
curl -fsSL https://herdr.dev/install.sh | sh
```

Already have Rust? Run `rustup update` to get 1.88 or newer.

## 2. Set up a Nerd Font

termcode draws its icons with a [Nerd Font](https://www.nerdfonts.com/font-downloads).

1. Download **JetBrainsMono Nerd Font** and install it (on Linux: unzip into
   `~/.local/share/fonts/`, then run `fc-cache -f`).
2. Pick the **Mono** variant in your terminal. For Ghostty, add this line to
   `~/.config/ghostty/config`:

   ```text
   font-family = "JetBrainsMono Nerd Font Mono"
   ```

Can't install a font? Use plain ASCII icons instead (see [Settings](#settings)):
`"termcode.icons": "ascii"`.

On a Mac connecting over SSH, install the font **on the Mac**: the terminal on your
screen draws the icons.

## 3. Build and install

```sh
git clone https://github.com/JonasAtelier/termcode.git
cd termcode
cargo install --path . --root ~/.local
```

This builds termcode (a few minutes the first time) and puts it at
`~/.local/bin/termcode`.

Make sure `~/.local/bin` is on your `PATH`:

```sh
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc   # zsh: ~/.zshrc
```

Open a new terminal and check:

```sh
termcode -h
```

**Low on memory?** Add `-j2` to the `cargo install` line to build with 2 jobs.

### Or: install it as a .deb package (Debian / Ubuntu)

Instead of `cargo install`, build a package and let apt install it to `/usr/bin`
(apt also installs git and ripgrep for you):

```sh
cargo install cargo-deb                             # once
cargo deb                                           # -> target/debian/termcode_*.deb
sudo apt install ./target/debian/termcode_*.deb
```

Pick one way: with both, the `~/.local/bin` copy usually wins. The package needs the same
or a newer Debian/Ubuntu than the machine that built it, so build it where you install it.

## 4. Add it to herdr (optional)

From the `termcode` folder:

```sh
herdr plugin link "$PWD"                 # .deb install: herdr plugin link /usr/share/termcode
herdr plugin list                        # shows jonas.termcode
```

herdr finds `termcode` through `PATH`. If herdr was already running before you changed
`PATH`, quit herdr and start it again.

## 5. Try it

```sh
cd ~/some/project
termcode                 # opens this folder; Ctrl+Q quits
```

Inside a herdr pane:

```sh
termcode --tab           # opens a "Code: project" tab
```

The new tab doesn't take focus: click **Code: project** in the tab bar.

## Updating

```sh
cd termcode
git pull
cargo install --path . --root ~/.local
# .deb install instead: cargo deb && sudo apt install ./target/debian/termcode_*.deb
```

That's all. A termcode that is already open reloads itself a few seconds later, keeping
its tabs and cursors. It waits while you have unsaved files. To reload by hand: F1, then
**Developer: Reload Window**.

## Uninstalling

```sh
herdr plugin unlink jonas.termcode
rm ~/.local/bin/termcode               # .deb install: sudo apt remove termcode
```

---

## Using it

### Opening things

```sh
termcode                    # this folder (its git repo if inside one)
termcode ~/proj             # any folder
termcode src/main.rs:120    # a file at line 120 (also :LINE:COL)
termcode --tab src          # the same, as a herdr tab
```

- **Quitting:** Ctrl+Q, or Close in the title bar. You are asked about unsaved files first.
- **Sessions:** open the same folder again and your tabs, cursors and layout come back.
  Unsaved text survives a crash too.
- **Subfolders:** `termcode drivers/net` shows only that folder, but Source Control still
  uses the whole repo.
- **Keyboard shortcut for the Code tab:** none is set. In herdr, bind `plugin_action`
  `jonas.termcode.open` to a free key.

### The sidebar

| View | Open with | What it does |
|---|---|---|
| Explorer | Alt+1, Ctrl+E | Files. **Timeline** below shows the open file's commits |
| Search | Alt+4, F4 | Search all files. **References** below shows where the symbol at the cursor is used |
| Source Control | Alt+2, F2, Ctrl+G | Changes, commits and the commit graph |

Click the active sidebar icon to hide the sidebar. Drag any divider to resize.

### Common keys

| Key | Does |
|---|---|
| F1 | Command palette (every command is here) |
| Ctrl+P | Open a file by name. Type `:42` to go to a line, `@` for a function |
| Ctrl+S / Ctrl+W / Ctrl+Q | Save / close tab / quit |
| Ctrl+Z / Ctrl+Y | Undo / redo |
| Ctrl+C / Ctrl+X / Ctrl+V | Copy / cut / paste |
| Ctrl+F / Ctrl+H | Find / replace in this file |
| Ctrl+/ | Comment or uncomment lines |
| Ctrl+Shift+Up / Down | Add a cursor above / below |
| Alt+Up / Alt+Down | Move lines up / down |
| Ctrl+Shift+[ / ] | Fold / unfold |
| Alt+Z | Toggle word wrap |
| Ctrl+K V | Markdown preview |
| F12 or Ctrl+click | Go to Definition |
| Shift+F12 | Find all references |
| Alt+← / Alt+→ | Go back / forward |
| F7 / Shift+F7 | Next / previous change in a diff |
| Ctrl+PgDn / Ctrl+PgUp | Next / previous tab |
| Ctrl+, | Settings |

**Mouse:** click a file to preview it (italic tab), double-click its tab to keep it open.
Double-click a name in the Explorer to rename it. Drag a file to an editor edge to split.
Rest the pointer on a function to see its docs.

Some terminals (GNOME Terminal, Ptyxis) can't send Ctrl+Shift keys, Ctrl+Tab or
Ctrl+1/2/3. Use F1 or the other key in the table instead.

### Source Control

- **Changes:** `+` stages a file, `−` unstages it, `↶` discards (always asks).
- **Commit:** click **Add commit**. Stage files, write a message, then **Commit** or
  **Commit & Push**. Your draft is kept if you cancel.
- **Graph:** click a commit to see its files, click a file to see its diff. Rest the pointer
  on a commit (or press `i`) for a card with **Copy hash** and **Open changes**.
- **Status bar:** click the branch to switch or create one; click `↓1 ↑2` to pull and push.
- **More Git:** F1, then type `Git:` for pull, push, fetch, stash, merge, rebase and more.
  Risky ones ask first.
- **Merge conflicts:** click **Accept Current**, **Accept Incoming** or **Accept Both** above
  each conflict, save, then `+` to mark it resolved.

Git runs in the background and never asks for a password inside the panel. If a command
hangs, click the spinner in the status bar to cancel it. F1 **Git: Show Git Output** shows
what ran.

---

## Settings

Press **Ctrl+,** (or click the gear at the bottom left). Change things, then:

- **Apply** to try them out,
- **Save** to keep them.

Prefer a file? The first time termcode starts it writes
**`~/.config/termcode/settings.json`**, listing every option of the Settings window at its
default, commented out. To change one, remove the `//` in front of it and edit the value;
saving applies it. F1 **Preferences: Open Settings (JSON)** opens the file from inside
termcode. A broken file never crashes the panel: the last good settings stay.

```jsonc
    // "termcode.icons": "nerd",           // Icons: "nerd", "ascii"
    "termcode.icons": "ascii",             // <- uncommented and changed
```

Lost track of the defaults? `termcode --default-config` prints the full list again.

A few more examples:

```jsonc
{
    "workbench.colorTheme": "Default Dark Modern",   // any VS Code theme you have installed
    "workbench.colorCustomizations": { "statusBar.background": "#3B3B3B" },
    "workbench.density": "compact",                  // tighter layout
    "termcode.icons": "ascii",                     // no Nerd Font needed
    "editor.tabSize": 8,
}
```

<details>
<summary>All settings</summary>

| Key | Default | Meaning |
|---|---|---|
| `workbench.colorTheme` | "Visual Studio Dark - C++" | A VS Code theme name, or a path to a theme JSON |
| `workbench.colorCustomizations` | `{}` | Override UI colours, also per theme (`"[Theme Name]": {…}`) |
| `editor.tokenColorCustomizations` | `{}` | Override code colours (`comments`, `strings`, `textMateRules`, …) |
| `termcode.followVSCode` | `false` | Take the three keys above from your VS Code settings |
| `workbench.density` | `"comfortable"` | or `"compact"` |
| `termcode.spacing` | per density | Fine-tune sizes, see below |
| `termcode.icons` | `"nerd"` | `"ascii"`: no Nerd Font needed |
| `termcode.blocks.style` | `"cards"` | `"cards"` rounded boxes, `"lines"` shared lines, `"none"` flat |
| `termcode.fileIcons` | `{}` | Custom file icons: `extensions`, `fileNames`, `folderNames` → `{ "glyph", "color" }` |
| `indentRainbow.enabled` | `true` | Colour each indent level |
| `indentRainbow.indicatorStyle` | `"classic"` | `"light"`: a thin line instead of a background |
| `indentRainbow.colors`, `.errorColor`, `.tabmixColor` | the extension's | Rainbow colours |
| `indentRainbow.excludedLanguages`, `.includedLanguages`, `.ignoreErrorLanguages`, `.ignoreLinePatterns` | as the extension | Where the rainbow applies |
| `editor.tabSize` | `4` | Tab width |
| `editor.guides.indentation` | `true` | Indent guide lines |
| `editor.renderWhitespace` | `"boundary"` | `"none"`, `"selection"`, `"trailing"`, `"all"` |
| `editor.occurrencesHighlight` | `"singleFile"` | Highlight the word under the cursor; `"off"` |
| `editor.matchBrackets` | `"always"` | Highlight matching brackets; `"never"` |
| `editor.bracketPairColorization.enabled` | `true` | Colour brackets by depth |
| `editor.wordWrap` | Markdown only | `"on"` or `"off"` for every file |
| `editor.autoClosingBrackets`, `editor.autoClosingQuotes` | `"always"` | Auto-close pairs; `"never"` |
| `editor.rulers` | `[90]` | Vertical lines at these columns; `[]` for none |
| `editor.minimap.enabled` | `true` | The code overview on the right |
| `editor.minimap.side` | `"right"` | or `"left"` |
| `editor.minimap.showSlider` | `"always"` | or `"mouseover"` |
| `editor.minimap.size` | `"fill"` | `"proportional"` or `"fit"` |
| `editor.minimap.renderCharacters` | `true` | `false`: blocks instead of dots |
| `editor.minimap.maxColumn` | `120` | Columns the minimap covers |
| `termcode.minimap.minEditorWidth` | `40` | Hide the minimap when the editor gets narrower |
| `editor.gotoLocation.multipleDefinitions` | `"peek"` | `"gotoAndPeek"` or `"goto"` |
| `breadcrumbs.enabled` | `true` | The path row under the tabs |
| `files.trimTrailingWhitespace` | `true` | On save, on edited lines |
| `files.insertFinalNewline` | `true` | On save |
| `workbench.tree.enableStickyScroll` | `true` | Keep the current folder pinned while scrolling |
| `workbench.tree.stickyScrollMaxItemCount` | `1` | More than 1: nested rows, like VS Code |
| `termcode.restoreSession` | `true` | Reopen last session's tabs |
| `workbench.editor.restoreViewState` | `true` | Restore cursor and scroll per file |
| `git.blame.statusBarItem.enabled` | `true` | Line author in the status bar |
| `git.blame.editorDecoration.enabled` | `false` | Line author after the cursor line |
| `git.confirmSync` | `true` | Ask before pull + push |

`termcode.spacing` keys (comfortable / compact): `sidebarWidth` 36/30,
`sidebarPadding` 2/0, `sectionGap` 1/0, `editorPadding` 0/0, `tabPadding` 2/1,
`statusBarGap` 3/3, `statusBarPadding` 2/1, `activityBarWidth` 5/3, `listItemGap` 0/0,
`popupPadding` [3,1]/[0,0], `iconGap` 2/1, `treeIndent` 2/2, `titleBarPadding` 2/1,
`buttonPadding` 2/1, `findWidgetPadding` 1/0.

Environment variables: `TERMCODE_ICONS=0` forces ASCII icons;
`TERMCODE_GRAPHICS=kitty|blocks|text` picks how Settings draws its controls.

</details>

## Where files are kept

| What | Where |
|---|---|
| Settings | `~/.config/termcode/settings.json` |
| Sessions, layout, unsaved-file backups, commit drafts | `~/.local/state/termcode/` |

Upgrading from herdr-code? Your settings and sessions are copied to these folders the
first time termcode starts.

## Troubleshooting

| Problem | Fix |
|---|---|
| Icons show as boxes or `?` | Use a Nerd Font **Mono** in your terminal (step 2), or set `"termcode.icons": "ascii"` |
| `termcode: command not found` | Add `~/.local/bin` to `PATH` (step 3) |
| The Code tab doesn't open | Restart herdr after fixing `PATH`; check `herdr plugin list` |
| Ctrl+Shift keys do nothing | Your terminal doesn't send them; use F1 or the other key |
| Build runs out of memory | `cargo install --path . --root ~/.local -j2` |
| A file opens read-only | It is over 1 MB / 20,000 lines, binary, or not UTF-8 |

## Known limits

- No language server: Go to Definition and References search the code with `rg`, so some
  definitions (C++ methods in a class body, macro-built names) are missed.
- Find matches inside one line only. Lines over 4000 bytes aren't syntax-coloured.
- Light themes that don't set every colour get some dark panels.
- Mouse back/forward buttons don't reach the panel (herdr drops them).

## Development

```sh
cargo run -- .     # run from source
cargo test         # run the tests
```

See [AGENTS.md](AGENTS.md) for the code layout and conventions.

## Credits

The default colours and `assets/vscode_dark_defaults.json` come from VS Code (MIT, Microsoft);
`assets/seti.json` from Seti UI (MIT, Jesse Weed). See `assets/vscode_LICENSE.txt`.
