#!/bin/sh
# usage: tools/lines.sh [file]   -- predicted board S for track A: lines of rustfmt's output that are not blank or comments, plus 6
# (checked against the board's S for three commits). Also leaves the formatted copy in .out/formatted.rs.
cd "$(dirname "$0")/.."
f=${1:-track-a/src/main.rs}; mkdir -p .out
sed 's/\r$//' "$f" | MSYS_NO_PATHCONV=1 docker run --rm -i sd-bench sh -c 'cat > /tmp/f.rs && rustfmt --edition 2021 --emit stdout /tmp/f.rs 2>/dev/null' > .out/formatted.rs
echo "formatted non-comment lines $(grep -vcE '^\s*(//|$)' .out/formatted.rs), predicted S $(( $(grep -vcE '^\s*(//|$)' .out/formatted.rs) + 6 ))"
