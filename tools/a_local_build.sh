#!/bin/sh
# usage: abuild.sh <name> <main.rs> -- Track A build as the board does it (track-a/.cargo flags, rustc 1.99.1), CPU raptorlake
set -e
src=$(realpath "$2"); w=/home/user/abuild/$1; mkdir -p $w/src
cp $(dirname "$0")/../track-a/Cargo.toml $(dirname "$0")/../track-a/Cargo.lock $w/; cp "$src" $w/src/main.rs
cd $w && RUSTFLAGS="-C target-cpu=${CPU:-raptorlake} -C target-feature=+crt-static -C relocation-model=static $EXTRA" cargo +1.99.1 build --release --offline -q 2>&1 | grep -E "^error" -A5 || true
cp target/release/solution /home/user/bins/$1
f=$(mktemp); rustfmt --edition 2021 --emit stdout "$src" > $f 2>/dev/null
echo "$1 built; S lines $(grep -vcE '^\s*(//|$)' $f)"
