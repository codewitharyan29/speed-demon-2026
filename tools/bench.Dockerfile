# Benchmark toolbox for tools/hyper.sh: the pinned build image plus hyperfine and strace.
# The Docker VM clock can be hours off, which makes apt reject signatures, so both tools are installed from local
# files (build context = .out/debs: the static hyperfine binary and strace_6.13+ds-1_amd64.deb from deb.debian.org).
FROM rust:1-slim-trixie@sha256:1a939748a7cbab172de91a014e3804a97af1394a8c844faa67fce9401c4e40fb
COPY hyperfine /usr/local/bin/hyperfine
COPY strace_6.13+ds-1_amd64.deb /tmp/strace.deb
COPY libunwind8_1.3.2-2_amd64.deb /tmp/libunwind.deb
RUN dpkg -i /tmp/libunwind.deb /tmp/strace.deb && rm /tmp/*.deb
