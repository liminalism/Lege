#!/usr/bin/env python3
"""Interleaved paired symbol-harness runs pinned to CPUs 2,3; prints per-preset median ratios and checks hashes."""
import subprocess, sys, os, random, statistics, json
def run(worker, d, presets, scope, rounds):
    env=dict(os.environ, SYMBOL_SPEED_DIR=d, SYMBOL_SPEED_PRESETS=presets, SYMBOL_SPEED_SCOPE=scope, SYMBOL_SPEED_ROUNDS=str(rounds))
    out=subprocess.run(['taskset','-c','2,3',worker,'--ignored','--exact','symbol_speed_worker','--nocapture'],env=env,capture_output=True,text=True).stdout
    res={}
    for line in out.splitlines():
        if 'SYMBOL_SPEED\t' in line:
            f=line[line.index('SYMBOL_SPEED'):].split('\t')
            res[f[1]]=(float(f[6]), int(f[7]), f[8])
    if not res: raise SystemExit(out[-2000:])
    return res
def main():
    base,cand,d,presets,scope,rounds,pairs=sys.argv[1:8]; rounds=int(rounds); pairs=int(pairs)
    rng=random.Random(11); ratios={}; times={'b':{}, 'c':{}}
    for p in range(pairs):
        order=[('b',base),('c',cand)]; rng.shuffle(order)
        res={}
        for k,w in order: res[k]=run(w,d,presets,scope,rounds)
        for preset in res['b']:
            b=res['b'][preset]; c=res['c'][preset]
            assert b[1]==c[1] and b[2]==c[2], f'output differs for {preset}: {b} vs {c}'
            ratios.setdefault(preset,[]).append(c[0]/b[0]); times['b'].setdefault(preset,[]).append(b[0]); times['c'].setdefault(preset,[]).append(c[0])
        print('pair',p,{k:round(v[-1],3) for k,v in ratios.items()},flush=True)
    for preset,r in ratios.items():
        print(json.dumps({'preset':preset,'scope':scope,'base_median_s_per_page':statistics.median(times['b'][preset]),'cand_median_s_per_page':statistics.median(times['c'][preset]),'median_ratio':statistics.median(r),'min':min(r),'max':max(r),'bytes_and_hash_identical':True}))
main()
