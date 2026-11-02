#!/bin/sh
# usage: sscore.sh <main.rs> -- exact S of a Track A source: scc code lines of rustfmt-default output + 7 (Dockerfile)
d=$(mktemp -d); cp "$1" $d/main.rs; rustfmt --edition 2021 $d/main.rs 2>/dev/null; n=$(/tmp/scc_dl/scc --format json $d 2>/dev/null | python3 -c "import json,sys; print(sum(x['Code'] for x in json.load(sys.stdin) if x['Name']=='Rust'))"); echo "rust_code=$n S=$((n+7))"; rm -rf $d
