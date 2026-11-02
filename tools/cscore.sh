#!/bin/sh
# usage: cscore.sh <main.cpp> -- exact S of a Track C source: scc code lines of clang-format 23.1.3 --style=LLVM output + 7 (Dockerfile)
d=$(mktemp -d); /tmp/cf23/x/clang_format/data/bin/clang-format --style=LLVM --assume-filename=main.cpp < "$1" > $d/main.cpp; n=$(/tmp/scc_dl/scc --format json $d 2>/dev/null | python3 -c "import json,sys; print(sum(x['Code'] for x in json.load(sys.stdin) if x['Name']=='C++'))"); echo "cpp_code=$n S=$((n+7))"; rm -rf $d
