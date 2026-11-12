#!/bin/sh
# The README for people who install termcode, from README.md: drops what is for building
# it (<!-- dev --> ... <!-- /dev --> blocks and table rows marked <!--dev-->), and shows
# the <!-- user ... user --> blocks.
#   scripts/user-readme.sh             # the copy the .deb installs
#   scripts/user-readme.sh --releases  # the termcode-releases front page (says no source)
note=""
[ "${1:-}" = "--releases" ] && note="> This repository is where termcode is published, and where you report problems and ask
> for features. It holds no source code."
awk -v note="$note" '
    /^<!-- dev -->$/  { skip = 1; next }
    /^<!-- \/dev -->$/ { skip = 0; next }
    /^<!-- user$/     { next }
    /^user -->$/      { next }
    /<!--dev-->/      { next }
    skip              { next }
    # The note goes after the first paragraph under the title.
    note != "" && !done && seen && $0 == "" { print; print note; done = 1 }
    /^# / { title = 1 }
    title && $0 != "" && !/^# / { seen = 1 }
    { print }
' "$(dirname "$0")/../README.md" | cat -s
