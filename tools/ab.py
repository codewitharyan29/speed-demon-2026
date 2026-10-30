"""Interleaved A/B timing of track variants on the full dataset (docker volume sddata).
usage: python tools/ab.py <track-dir-A> <track-dir-B> [...] [-n 9] [-t d|b]
Each dir is a cargo project like track-d/. A timer is injected before the output write, and runs alternate between
variants so machine noise hits all of them equally. Prints min / median of the in-process time in ms."""
import os, re, shutil, statistics, subprocess, sys, tempfile, time
env = {**os.environ, "MSYS_NO_PATHCONV": "1"}
args = sys.argv[1:]; n = 9; T = "d"
if "-t" in args: i = args.index("-t"); T = args[i + 1]; del args[i:i + 2]
OUT = {"d": "distances.txt", "b": "results.txt"}[T]
if "-n" in args: i = args.index("-n"); n = int(args[i + 1]); del args[i:i + 2]
tags = []
for k, d in enumerate(args):
    tmp = os.path.join(tempfile.gettempdir(), f"sdab{k}"); shutil.rmtree(tmp, ignore_errors=True)
    shutil.copytree(d, tmp, ignore=shutil.ignore_patterns("target"))
    p = os.path.join(tmp, "src", "main.rs"); s = open(p).read().replace("\r\n", "\n")
    s = s.replace("fn main() {\n", "fn main() {\n    let t0 = std::time::Instant::now();\n", 1)
    write = f'std::fs::write("/out/{OUT}"'
    s = s.replace(write, 'eprintln!("end {}", t0.elapsed().as_micros());\n    ' + write, 1)
    open(p, "w", newline="\n").write(s)
    subprocess.run(["docker", "build", "-q", "--network=none", "-t", f"sd-ab{k}", tmp], check=True, stdout=subprocess.DEVNULL)
    tags.append(f"sd-ab{k}")
res = {t: [] for t in tags}
for _ in range(n):
    for t in tags:
        r = subprocess.run(["docker", "run", "--rm", "--cpuset-cpus=0-11", "--mount", f"type=volume,src=sddata,dst=/data,volume-subpath={T},readonly",
                            "-v", os.path.join(os.getcwd(), ".out", f"new-{T}") + ":/out", t], env=env, capture_output=True, text=True)
        res[t].append(int(re.search(r"end (\d+)", r.stderr).group(1)) / 1000)
for t, d in zip(tags, args):
    v = res[t]; print(f"{d}: min {min(v):.0f} ms  median {statistics.median(v):.0f} ms  all {[round(x) for x in v]}")
