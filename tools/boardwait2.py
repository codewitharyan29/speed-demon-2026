import json,sys,time,urllib.request
slug,commit,limit=sys.argv[1],sys.argv[2],float(sys.argv[3])
U="https://s3.amazonaws.com/raptors.dev/speeddemon.dev/2026/board/board.json"
t0=time.time()
while time.time()-t0<limit:
    try: d=json.load(urllib.request.urlopen(U))
    except Exception: time.sleep(10); continue
    t=[x for x in d['teams'] if x['slug']==slug][0]
    res={}
    for tr in 'abcd':
        l=t['tracks'].get(tr,{}).get('latest')
        if l and l['commit']==commit and l.get('ended_at'): res[tr]=l
    if 'a' in res and 'c' in res:
        print(json.dumps(res)); sys.exit(0)
    time.sleep(20)
print("TIMEOUT"); sys.exit(1)
