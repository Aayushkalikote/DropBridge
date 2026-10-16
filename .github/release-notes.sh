#!/bin/bash
# Print the release notes for a tag from the README changelog, entries sorted by type.
# Prints nothing if the tag has no "### <tag>" section; fails on an entry without a known type.
# Usage: .github/release-notes.sh v0.1.0
set -euo pipefail
tag=$1
types="Breaking Feature Enhancement Fix Security Docs" # release-note order

awk -v tag="$tag" -v types="$types" '
  BEGIN { n = split(types, t, " "); for (i = 1; i <= n; i++) rank[t[i]] = i }
  /^## / && found { exit }
  /^### / { if (found) exit; split($0, h, " "); if (h[2] == tag) found = 1; next }
  !found || !/[^[:space:]]/ || /^</ { next }  # skip blanks and the <details> wrapper
  {
    if (match($0, /^- [A-Za-z]+: /)) {
      type = substr($0, 3, RLENGTH - 4)
      if (type in rank) {
        printf "%d\t%06d\t- **%s:** %s\n", rank[type], NR, type, substr($0, RLENGTH + 1)
        next
      }
    }
    print "changelog entry must start with \"- <Type>: \" (" types "): " $0 > "/dev/stderr"
    bad = 1
  }
  END { exit bad }
' "$(dirname "$0")/../README.md" | sort -t "$(printf '\t')" -k1,1n -k2,2n | cut -f3-
