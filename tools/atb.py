import os,sys,time,subprocess,statistics as st
cpus=sys.argv[1]; rounds=int(sys.argv[2]); data=sys.argv[3]; bins=sys.argv[4:]
r={b:[] for b in bins}; c={b:[] for b in bins}
for i in range(rounds):
    for b in (bins[i%len(bins):]+bins[:i%len(bins)]):
        subprocess.run(["rm","-rf","/tmp/aout2"]); os.makedirs("/tmp/aout2")
        t=time.perf_counter(); p=subprocess.Popen(["taskset","-c",cpus,"/home/user/bins/"+b,"--data",data,"--out","/tmp/aout2"]); _,_,ru=os.wait4(p.pid,0); r[b].append(time.perf_counter()-t); c[b].append(ru.ru_utime+ru.ru_stime)
for b in bins: print(b,"wall med %.2f ms cpu med %.2f ms (user+sys)"%(st.median(r[b])*1e3,st.median(c[b])*1e3))
