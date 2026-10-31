"""Wall-time ablation of Track B's corpus scan: python tools/ablate.py   (needs the docker volume sddata)
Replaces the body of the per-batch closure with progressively more of the real work, times the parallel scan only."""
import os, shutil, subprocess, tempfile
base = open("track-b/src/main.rs").read().replace("\r\n", "\n")
a = base.index("            scan(docs, range, base, true, &mut offs, |batch| {")
b = base.index("            (pairs, offs, base)\n")
key = "let k = key(docs, t.at as usize, t.len as usize);"
bodies = {
    "A tokenise only": "for t in batch { n += t.len as u64; } true",
    "B +key+hash": "for t in batch.iter().filter(|t| t.len <= 32) { " + key + " n += hash(&k) & 1; } true",
    "C +probe": "for t in batch.iter().filter(|t| t.len <= 32) { " + key + " if terms.find(hash(&k), &k).is_ok() { n += 1; } } true",
}
for name, body in bodies.items():
    s = base[:a] + "            let mut n = 0u64;\n            scan(docs, range, base, true, &mut offs, |batch| { " + body + " });\n            pairs.push(n as u32 & 1);\n" + base[b:]
    c = s.index("    let scanned = par(")
    s = s[:c] + "    let t0 = std::time::Instant::now();\n" + s[c:]
    d = s.index("    // Posting lists in one array.")
    s = s[:d] + '    eprintln!("%s: {:?}", t0.elapsed());\n    std::process::exit(0);\n' % name + s[d:]
    tmp = os.path.join(tempfile.gettempdir(), "sdabl"); shutil.rmtree(tmp, ignore_errors=True)
    shutil.copytree("track-b", tmp, ignore=shutil.ignore_patterns("target"))
    open(os.path.join(tmp, "src", "main.rs"), "w", newline="\n").write(s)
    r = subprocess.run(["docker", "build", "-q", "--network=none", "-t", "sd-abl", tmp], capture_output=True, text=True)
    if r.returncode: print(name, "BUILD FAIL", r.stderr[-800:]); continue
    for _ in range(3):
        subprocess.run(["docker", "run", "--rm", "--cpuset-cpus=0-11", "--mount", "type=volume,src=sddata,dst=/data,volume-subpath=b,readonly",
                        "-v", os.getcwd() + "/.out/new-b:/out", "sd-abl"], env={**os.environ, "MSYS_NO_PATHCONV": "1"})
