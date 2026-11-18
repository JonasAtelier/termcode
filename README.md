# termcode

Edit code and review Git changes in a Linux terminal. Open a project to browse its files,
search across them, and see edits side by side with the last commit. You can also open the
editor as a Code tab in herdr.

<!-- dev -->
**Contents** · [Run from source](#run-from-source) · [Edit a file](#edit-a-file) · [Review changes](#review-changes) · [Serial ports](#serial-ports) · [Scripts and AI agents](#scripts-and-ai-agents) · [Help and settings](#help-and-settings) · [License and credits](#license-and-credits)
<!-- /dev -->
<!-- user
**Contents** · [Install on Debian or Ubuntu](#install-on-debian-or-ubuntu) · [Edit a file](#edit-a-file) · [Review changes](#review-changes) · [Serial ports](#serial-ports) · [Scripts and AI agents](#scripts-and-ai-agents) · [Help and settings](#help-and-settings) · [License and credits](#license-and-credits)
user -->

![termcode showing the file explorer, editor and integrated terminal](doc/screenshots/editor.png)

<!-- dev -->
## Run from source

On x86_64 or arm64 Linux, install Rust 1.93 or newer and a C linker. Install Git for
source control and ripgrep (`rg`) for project search. Run the editor from the repository root.

```sh
cargo run -- .
```

Install the published crate with `cargo install termcode`. To install this checkout, run `cargo install --locked --path .`
and put Cargo’s bin directory (normally `~/.cargo/bin`) on your PATH. From this
checkout, `herdr plugin link "$PWD"` adds the optional integration for herdr 0.9.1+.

Packaged binaries are available from [Releases](https://github.com/JonasAtelier/termcode-releases/releases).
<!-- /dev -->
<!-- user
## Install on Debian or Ubuntu

Run `dpkg --print-architecture` to find your architecture (`amd64` or `arm64`).
Download the matching `.deb` from the [latest release](https://github.com/JonasAtelier/termcode-releases/releases/latest).
In the folder containing that package, install it and check the version.

```sh
sudo apt install ./termcode_*.deb
termcode --version
```

The release notes also explain how to add the signed apt repository for updates.
For a Code tab in herdr 0.9.1+, run `herdr plugin link /usr/share/termcode`.
user -->

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

The **Serial** view (the plug above the Programmer) lists the serial ports with their
adapter, USB ID and who holds each, and the channels open here with their state. Click a
port to open it: its output streams in an editor tab you can split like code, and what
you type there goes to the device. Baud, DTR, RTS, break, capture and close are under
each channel. Ports are taken as [xcomm](https://crates.io/crates/xcomm) takes them (a
UUCP lock, an exclusive open) and opened again after an unplug.

## Scripts and AI agents

`termcode api` drives a running termcode: run any F1 command, read open files and their
problems, edit as one undo step, type into its terminal. Reading needs no approval;
before a program changes files or runs anything, termcode asks you (Allow, Allow once,
Deny). `termcode api --help` lists the methods and `termcode api --skill` prints the
guide for agents. Set `termcode.api` to `"read-only"` or `"off"` in Settings to limit it.

## Help and settings

**Ctrl+,** opens Settings. The Debian package includes `man termcode`. The
[0.4.0 user guide](https://github.com/JonasAtelier/termcode-releases/blob/6fcfc0078c2f0a48eac95b96a0446457dde30b53/README.md)
covers shortcuts, previews, optional tools and troubleshooting. To report a problem,
run **Help: Report Issue** from F1 and review the copied diagnostics before posting
to the [issue tracker](https://github.com/JonasAtelier/termcode-releases/issues/new/choose).

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
