#!/usr/bin/env python3
"""Interleaved paired runs of generic kernel workers pinned to CPU 0; prints per-pair ratios and medians."""
import subprocess, sys, os, random, statistics, json
S=os.path.dirname(os.path.abspath(__file__))
def run(worker, inp, rounds):
    env=dict(os.environ, GENERIC_INPUT=inp, GENERIC_ROUNDS=str(rounds))
    out=subprocess.run(['taskset','-c','0',worker,'--ignored','--exact','generic_kernel_worker','--nocapture'],env=env,capture_output=True,text=True).stdout
    for line in out.splitlines():
        if 'GENERIC_RESULT' in line:
            f=line[line.index('GENERIC_RESULT'):].split('\t'); return float(f[1])/int(f[3])
    raise SystemExit(out)
def main():
    base, cand, inp, rounds, pairs = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4]), int(sys.argv[5])
    rng=random.Random(7); ratios=[]; bt=[]; ct=[]
    for p in range(pairs):
        order=[('b',base),('c',cand)]; rng.shuffle(order)
        res={}
        for k,w in order: res[k]=run(w,inp,rounds)
        bt.append(res['b']); ct.append(res['c']); ratios.append(res['c']/res['b'])
        print(f"pair {p}: base {res['b']:.4f} cand {res['c']:.4f} ratio {res['c']/res['b']:.4f}", flush=True)
    print(json.dumps({'input':os.path.basename(inp),'rounds':rounds,'pairs':pairs,'base_median':statistics.median(bt),'cand_median':statistics.median(ct),'median_ratio':statistics.median(ratios),'min_ratio':min(ratios),'max_ratio':max(ratios),'ratios':ratios,'base':bt,'cand':ct}))
main()
