#!/bin/sh
# The README for people who install termcode, from README.md: drops what is for building
# it (<!-- dev --> ... <!-- /dev --> blocks and table rows marked <!--dev-->), and shows
# the <!-- user ... user --> blocks.
#   scripts/user-readme.sh             # the copy the .deb installs
awk '
    /^<!-- dev -->$/  { skip = 1; next }
    /^<!-- \/dev -->$/ { skip = 0; next }
    /^<!-- user$/     { next }
    /^user -->$/      { next }
    /<!--dev-->/      { next }
    skip              { next }
    { print }
' "$(dirname "$0")/../README.md" | cat -s
