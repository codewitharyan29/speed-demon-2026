#!/bin/sh
# Local helper (session 4): fixtures in /home/user/cdata/<name> (+ <name>.ref), binaries in /home/user/bins.
# usage: build.sh <name> <main.rs>  -> /home/user/bins/<name>; builds for the board's CPU (Raptor Lake, AVX2, no AVX-512)
set -e
src=$(realpath "$2"); w=/home/user/build/$1; mkdir -p $w/src
cp /home/user/speed-demon-2026/track-c/Cargo.toml $w/; cp "$src" $w/src/main.rs
cd $w && RUSTFLAGS="-C target-cpu=${CPU:-raptorlake} $EXTRA" cargo +${TC:-1.99.1} build --release --offline -q 2>&1 | grep -E "^(error|warning: unused)" || true
cp target/release/solution /home/user/bins/$1
f=$(mktemp); rustfmt --edition 2021 --emit stdout "$src" > $f 2>/dev/null
echo "$1 built; S lines (non-blank non-comment after rustfmt) $(grep -vcE '^\s*(//|$)' $f)"
