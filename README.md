# termcode

Edit code and review Git changes in a Linux terminal. Open a project to browse its files,
search across them, and see edits side by side with the last commit. You can also open the
editor as a Code tab in herdr.

<!-- dev -->
**Contents** · [Install](#install) · [Run from source](#run-from-source) · [Edit a file](#edit-a-file) · [Review changes](#review-changes) · [Serial ports](#serial-ports) · [Scripts and AI agents](#scripts-and-ai-agents) · [Help and settings](#help-and-settings) · [License and credits](#license-and-credits)
<!-- /dev -->
<!-- user
**Contents** · [Install](#install) · [Edit a file](#edit-a-file) · [Review changes](#review-changes) · [Serial ports](#serial-ports) · [Scripts and AI agents](#scripts-and-ai-agents) · [Help and settings](#help-and-settings) · [License and credits](#license-and-credits)
user -->

![termcode showing the file explorer, editor and integrated terminal](doc/screenshots/editor.png)

<!-- dev -->
## Run from source

On x86_64 or arm64 Linux, install Rust 1.93 or newer and a C linker. Install Git for
source control and ripgrep (`rg`) for project search. Run the editor from the repository root.

```sh
cargo run -- .
```

To install this checkout, run `cargo install --locked --path .`
and put Cargo’s bin directory (normally `~/.cargo/bin`) on your PATH. From this
checkout, `herdr plugin link "$PWD"` adds the optional integration for herdr 0.9.1+.
<!-- /dev -->

## Install

On Debian or Ubuntu (amd64 or arm64), add termcode's signed apt repository; new versions
then come with `sudo apt upgrade`.

```sh
sudo curl -fsSLo /usr/share/keyrings/termcode.gpg https://jonasatelier.github.io/termcode/termcode.gpg
echo "deb [signed-by=/usr/share/keyrings/termcode.gpg] https://jonasatelier.github.io/termcode stable main" \
  | sudo tee /etc/apt/sources.list.d/termcode.list
sudo apt update && sudo apt install termcode
```

Or download the `.deb` for your machine (`dpkg --print-architecture` says `amd64` or
`arm64`) from the [latest release](https://github.com/JonasAtelier/termcode/releases/latest)
and run `sudo apt install ./termcode_*.deb`. For a Code tab in herdr 0.9.1+, run
`herdr plugin link /usr/share/termcode`.

With Rust 1.93 or newer, on any Linux: `cargo install termcode --locked`.

<a id="open-your-first-file"></a>

## Edit a file

From a project folder, run `termcode .` (or `termcode --tab .` in herdr). Press
**Ctrl+P**, type part of a filename and select it. **Ctrl+S** saves, **F1** lists
commands, and **Ctrl+Q** returns to the shell.

| Task | How to do it |
| --- | --- |
| Open a file at a known line | Run `termcode README.md:12` from the folder containing that file. |
| Find a function | Press **Ctrl+P** in the editor and type `@` to browse functions in the current file. |
| Preview Markdown | Open a Markdown file and press **Ctrl+Shift+V** to switch between source and preview. |

## Review changes

Save an edit, press **Ctrl+G** to open **Source Control**, and select the changed
file. Read the old and new versions side by side.

When a repository’s Git settings can start external programs, Source Control asks you
to trust the repository before enabling Git. Only grant trust if you know its origin.

## Serial ports

The **Serial (xcomm)** extension: click **+** above Settings, then Install beside it
under SUGGESTED (or `termcode ext install https://github.com/JonasAtelier/termcode#extensions/xcomm`;
it builds with cargo). Its view: the ports and the open channels. **Monitor** a port
to watch it (read only) in a tab you can split like code; **Attach** turns typing on;
its line settings (baud, format, DTR/RTS, break, capture, agents) sit on the right.
Ports are taken as [xcomm](https://crates.io/crates/xcomm) takes them (a UUCP lock, an
exclusive open) and opened again after an unplug; a port another xcomm holds is shared
with it. Dots: green reads and writes, yellow is on hold (it still reads), red is
disconnected. ✎ writes a note beside a port ("router console"); rest the pointer on a
port for everything about it. Agents drive ports through termcode's `serial.*` API or
with `xcomm ctl`.

## Scripts and AI agents

`termcode api` drives a running termcode: run any F1 command, read open files and their
problems, edit as one undo step, type into its terminal. Reading needs no approval;
before a program changes files or runs anything, termcode asks you (Allow, Allow once,
Deny). `termcode api --help` lists the methods and `termcode api --skill` prints the
guide for agents. Set `termcode.api` to `"read-only"` or `"off"` in Settings to limit it.
`termcode mcp` serves the same as MCP tools; `termcode --headless` runs one with no
terminal (`--api-stdio`: the API on stdin and stdout).

## Extensions

An extension adds its own view: an activity bar icon, side bar sections, a tab, right
and bottom panels. termcode draws and lays them out; the extension, a program in any
language, fills them with rows and handles clicks and keys.

Click **+** above Settings to install one (a git URL or a folder), turn any view off,
or remove one. From a shell:

```bash
termcode ext install https://github.com/someone/termcode-thing   # shows what it may do, asks
termcode ext list
termcode ext remove thing
termcode ext new mine                                             # a start for your own
```

Writing one: [doc/extensions.md](doc/extensions.md); for agents, `termcode ext --skill`.

## Help and settings

**Ctrl+,** opens Settings. The Debian package includes `man termcode`. The
[0.4.0 user guide](https://github.com/JonasAtelier/termcode/blob/v0.4.0/README.md)
covers shortcuts, previews, optional tools and troubleshooting. To report a problem,
run **Help: Report Issue** from F1 and review the copied diagnostics before posting
to the [issue tracker](https://github.com/JonasAtelier/termcode/issues/new/choose).

<!-- dev -->
Developers can find the test commands and release workflow in [AGENTS.md](AGENTS.md)
and version history in the [changelog](CHANGELOG.md).
<!-- /dev -->

## License and credits

<!-- dev -->
The current source is available under the [MIT license](LICENSE).
<!-- /dev -->
For packaged releases, see the terms shipped in `/usr/share/doc/termcode/copyright` and the dependency licenses in `/usr/share/doc/termcode/THIRD-PARTY-LICENSES`. Default colors come from Microsoft’s
VS Code and file icons from Jesse Weed’s Seti UI, both under MIT.
