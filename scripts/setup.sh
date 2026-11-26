#!/bin/sh
# termcode setup: what termcode's features use, set up as on the machine termcode is
# made on. Debian and Ubuntu (apt). Each part says what it does and asks first (Enter
# takes the default in brackets); what is already there is left alone.
#
#   setup.sh             ask for each part
#   setup.sh --yes       take every default without asking
#   setup.sh --all       yes to every part, the large ones too (ESP-IDF: about 2 GB)
#   setup.sh --dry-run   only show what would run
#
# It changes nothing of yours beyond what it names: no keybindings, themes or shells.
set -eu

YES=0 ALL=0 DRY=0
for a in "$@"; do
    case "$a" in
        --yes | -y) YES=1 ;;
        --all) ALL=1 YES=1 ;;
        --dry-run | -n) DRY=1 ;;
        -h | --help)
            sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "setup.sh: unknown option $a (see --help)" >&2
            exit 2
            ;;
    esac
done

DONE="" SKIPPED="" LATER=""
note() { printf '\n\033[1m%s\033[0m\n' "$*"; }
have() { command -v "$1" >/dev/null 2>&1; }

# Shows the command, runs it unless --dry-run.
run() {
    printf '  $ %s\n' "$*"
    [ "$DRY" = 1 ] || sh -c "$*"
}

# ask QUESTION DEFAULT(y|n) [large]: yes or no, from the terminal even when piped.
ask() {
    if [ "$ALL" = 1 ]; then return 0; fi
    if [ "$YES" = 1 ]; then [ "$2" = y ]; return; fi
    hint="[y/N]"
    [ "$2" = y ] && hint="[Y/n]"
    printf '%s %s ' "$1" "$hint"
    read -r reply </dev/tty || reply=""
    case "$reply" in
        [Yy]*) return 0 ;;
        [Nn]*) return 1 ;;
        *) [ "$2" = y ] ;;
    esac
}

part() { # part NAME QUESTION DEFAULT: asks; records what was skipped
    if ask "$2" "$3"; then return 0; fi
    SKIPPED="$SKIPPED $1"
    return 1
}

apt_install() { # the packages not there yet
    missing=""
    for p in "$@"; do
        dpkg -s "$p" >/dev/null 2>&1 || missing="$missing $p"
    done
    if [ -n "$missing" ]; then
        run "sudo apt-get install -y$missing"
    fi
}

if ! have apt-get; then
    echo "setup.sh is for Debian and Ubuntu (apt). Elsewhere, see the README's Install section." >&2
    exit 1
fi
[ "$DRY" = 1 ] && note "Dry run: nothing is installed or changed."

# ---- termcode itself (its .deb brings git, ripgrep, xclip, shellcheck)
if have termcode; then
    note "termcode: $(termcode --version 2>/dev/null) is installed."
else
    note "termcode: its apt repository, then the package (git, ripgrep, xclip, shellcheck with it)."
    if part termcode "Install termcode?" y; then
        run "sudo curl -fsSLo /usr/share/keyrings/termcode.gpg https://jonasatelier.github.io/termcode/termcode.gpg"
        run "echo 'deb [signed-by=/usr/share/keyrings/termcode.gpg] https://jonasatelier.github.io/termcode stable main' | sudo tee /etc/apt/sources.list.d/termcode.list >/dev/null"
        run "sudo apt-get update"
        apt_install termcode
        DONE="$DONE termcode"
    fi
fi

# ---- the tools termcode's views run
note "Tools: clang (C/C++ error checking), binutils (readelf: .so and ELF files), poppler-utils (PDF pages and text), unzip."
if have clang && have readelf && have pdftoppm && have unzip; then
    echo "  all there."
elif part tools "Install them?" y; then
    apt_install clang binutils poppler-utils unzip
    DONE="$DONE tools"
fi

# ---- GitHub CLI, for the GitHub view
if have gh; then
    note "GitHub CLI: $(gh --version | head -n 1) is installed."
else
    note "GitHub CLI (gh): the GitHub view reaches GitHub through it (cli.github.com's apt repository)."
    if part gh "Install gh?" y; then
        run "sudo curl -fsSLo /usr/share/keyrings/githubcli-archive-keyring.gpg https://cli.github.com/packages/githubcli-archive-keyring.gpg"
        run "echo \"deb [arch=\$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main\" | sudo tee /etc/apt/sources.list.d/github-cli.list >/dev/null"
        run "sudo apt-get update"
        apt_install gh
        DONE="$DONE gh"
        LATER="$LATER\n  - Sign in to GitHub: termcode's GitHub view has the button (or run: gh auth login)."
    fi
fi

# ---- a Nerd Font, for VS Code's icons in any terminal (Ghostty has them built in)
if fc-list 2>/dev/null | grep -qi "JetBrainsMono Nerd"; then
    note "Font: JetBrainsMono Nerd Font is installed."
else
    note "Font: JetBrainsMono Nerd Font into ~/.local/share/fonts (icons as in VS Code; Ghostty needs none)."
    if part font "Install it?" y; then
        apt_install unzip fontconfig
        run "tmp=\$(mktemp -d) && curl -fsSLo \"\$tmp/f.zip\" https://github.com/ryanoasis/nerd-fonts/releases/latest/download/JetBrainsMono.zip && mkdir -p \"\$HOME/.local/share/fonts/JetBrainsMonoNerd\" && unzip -oq \"\$tmp/f.zip\" '*.ttf' -d \"\$HOME/.local/share/fonts/JetBrainsMonoNerd\" && rm -rf \"\$tmp\" && fc-cache -f"
        DONE="$DONE font"
        LATER="$LATER\n  - Pick \"JetBrainsMono Nerd Font\" as your terminal's font (not needed in Ghostty)."
    fi
fi

# ---- Ghostty, the terminal termcode is made in (pictures at full resolution)
if have ghostty || snap list ghostty >/dev/null 2>&1; then
    note "Ghostty is installed."
else
    note "Ghostty: the terminal termcode is made and tested in; it shows pictures and PDFs sharply (snap)."
    if part ghostty "Install Ghostty?" y; then
        have snap || apt_install snapd
        run "sudo snap install ghostty --classic"
        DONE="$DONE ghostty"
    fi
fi

# ---- herdr, if you use it (termcode needs none): its Code tab and pictures
HERDR_CONF="${XDG_CONFIG_HOME:-$HOME/.config}/herdr/config.toml"
if have herdr; then
    note "herdr: termcode as a Code tab, its pictures passed on to the terminal."
    # Pictures (and PDFs) in a Code tab need herdr to pass them on.
    if grep -qs '^[[:space:]]*kitty_graphics[[:space:]]*=[[:space:]]*true' "$HERDR_CONF"; then
        echo "  herdr shows pictures already (kitty_graphics = true)."
    elif ask "Let herdr show termcode's pictures ([experimental] kitty_graphics = true)?" y; then
        if grep -qs '^[[:space:]]*kitty_graphics[[:space:]]*=' "$HERDR_CONF"; then
            # Set to false: turned on where it is (a second one would break the file).
            run "sed -i 's/^\\([[:space:]]*kitty_graphics[[:space:]]*=\\).*/\\1 true/' \"$HERDR_CONF\""
        elif grep -qs '^\[experimental\]' "$HERDR_CONF"; then
            run "sed -i '/^\\[experimental\\]/a kitty_graphics = true' \"$HERDR_CONF\""
        else
            run "mkdir -p \"$(dirname "$HERDR_CONF")\" && printf '\\n[experimental]\\nkitty_graphics = true\\n' >> \"$HERDR_CONF\""
        fi
        LATER="$LATER\n  - Apply herdr's config: herdr server reload-config (or restart herdr)."
    fi
    if [ -f /usr/share/termcode/herdr-plugin.toml ] && ! herdr plugin list 2>/dev/null | grep -q jonasatelier.termcode; then
        run "herdr plugin link /usr/share/termcode"
    fi
fi

# ---- serial ports, for the Programmer's Serial Console and uploads
if id -nG | tr ' ' '\n' | grep -qx dialout; then
    note "Serial ports: you are in the dialout group."
else
    note "Serial ports: the Programmer's Serial Console and uploads open /dev/ttyACM*, /dev/ttyUSB* (the dialout group)."
    if part serial "Add $(id -un) to dialout?" y; then
        run "sudo usermod -aG dialout $(id -un)"
        DONE="$DONE serial"
        LATER="$LATER\n  - Log out and in again: the dialout group counts from the next login."
    fi
fi

# ---- the Programmer's tools
note "Arduino: arduino-cli (Arduino's installer, into ~/.local/bin) and the AVR boards' core (Uno, Nano, Mega)."
if have arduino-cli; then
    echo "  arduino-cli is installed: $(arduino-cli version 2>/dev/null | head -n 1)"
elif part arduino "Install arduino-cli?" n; then
    run "mkdir -p \"\$HOME/.local/bin\" && curl -fsSL https://raw.githubusercontent.com/arduino/arduino-cli/master/install.sh | BINDIR=\"\$HOME/.local/bin\" sh"
    run "\"\$HOME/.local/bin/arduino-cli\" core update-index && \"\$HOME/.local/bin/arduino-cli\" core install arduino:avr"
    DONE="$DONE arduino"
    LATER="$LATER\n  - Other boards: arduino-cli core install esp32:esp32 (ESP32), rp2040:rp2040 … (their URLs: arduino-cli config add board_manager.additional_urls …)."
fi

note "ESP-IDF v5.5: Espressif's framework into ~/esp/esp-idf, then its toolchains (a large download, about 2 GB)."
if [ -f "$HOME/esp/esp-idf/export.sh" ] || [ -n "${IDF_PATH:-}" ]; then
    echo "  ESP-IDF is there."
elif part esp-idf "Install ESP-IDF?" n; then
    apt_install git wget flex bison gperf python3 python3-pip python3-venv cmake ninja-build ccache libffi-dev libssl-dev dfu-util libusb-1.0-0
    run "mkdir -p \"\$HOME/esp\" && git clone --depth 1 -b v5.5 --recursive --shallow-submodules https://github.com/espressif/esp-idf.git \"\$HOME/esp/esp-idf\""
    run "\"\$HOME/esp/esp-idf/install.sh\" all"
    DONE="$DONE esp-idf"
fi

note "STM32CubeProgrammer: ST asks for a sign-in to download it, so this part is yours."
if have STM32_Programmer_CLI || [ -x "$HOME/STMicroelectronics/STM32Cube/STM32CubeProgrammer/bin/STM32_Programmer_CLI" ]; then
    echo "  it is installed."
else
    LATER="$LATER\n  - STM32: download STM32CubeProgrammer for Linux from https://www.st.com/en/development-tools/stm32cubeprog.html, then termcode's Programmer view (STM32Cube) installs it from ~/Downloads."
fi

note "Done."
[ -n "$DONE" ] && echo "  set up:$DONE"
[ -n "$SKIPPED" ] && echo "  skipped:$SKIPPED (run setup.sh again for them)"
if [ -n "$LATER" ]; then
    printf '\nStill to do:%b\n' "$LATER"
fi
