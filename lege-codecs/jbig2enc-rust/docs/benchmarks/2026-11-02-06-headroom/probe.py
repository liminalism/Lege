import argparse,subprocess,os,json,random,re,statistics,hashlib
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--candidate',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--pairs',type=int,default=5);a=p.parse_args();a.output.mkdir(exist_ok=True)
root=Path('.agent/scratch/jbig2-perf-followup').resolve();baseline=Path('.agent/scratch/jbig2-06-headroom/baseline-worker').resolve();rows=[]
for corpus,path,rounds in [('mixed17',root/'final17/packed-input.bin',32),('noise',root/'final-noise/packed-input.bin',32)]:
 env=os.environ.copy();env.pop('GENERIC_PROFILE',None);env.pop('GENERIC_OUTPUT',None);env.update(GENERIC_INPUT=str(path),GENERIC_ROUNDS=str(rounds));rng=random.Random(20261103)
 for pair in range(a.pairs):
  order=['baseline','candidate'];rng.shuffle(order)
  for role in order:
   worker=baseline if role=='baseline' else a.candidate.resolve();stem=f'{corpus}-{pair}-{role}';counter=a.output/(stem+'.csv')
   command=['perf','stat','-x,','-e','cpu_core/cycles/u,cpu_core/instructions/u,task-clock','-o',str(counter),'--','taskset','-c','0',str(worker),'--ignored','--exact','generic_kernel_worker','--nocapture']
   log=a.output/(stem+'.log')
   if counter.exists() and log.exists():
    stdout=log.read_text()
   else:
    result=subprocess.run(command,env=env,capture_output=True,text=True);log.write_text(result.stdout+result.stderr);result.check_returncode();stdout=result.stdout
   match=re.search(r'GENERIC_RESULT\t([0-9.]+)\t(\d+)',stdout);assert match
   metrics={}
   for line in counter.read_text().splitlines():
    fields=line.split(',')
    if len(fields)>2 and fields[2] in ['cpu_core/cycles/u','cpu_core/instructions/u','task-clock']:metrics[fields[2]]=float(fields[0])
   assert len(metrics)==3,metrics
   rows.append({'corpus':corpus,'pair':pair,'role':role,'rounds':rounds,'kernel_seconds':float(match[1]),'bytes':int(match[2]),**metrics})
summary={}
for corpus in ['mixed17','noise']:
 summary[corpus]={}
 for metric in ['kernel_seconds','cpu_core/cycles/u','cpu_core/instructions/u','task-clock']:
  ratios=[next(r[metric] for r in rows if r['corpus']==corpus and r['pair']==i and r['role']=='candidate')/next(r[metric] for r in rows if r['corpus']==corpus and r['pair']==i and r['role']=='baseline') for i in range(a.pairs)]
  summary[corpus][metric]={'ratios':ratios,'median':statistics.median(ratios),'range':[min(ratios),max(ratios)]}
report={'method':'Partial system load unchanged. Same CPU0, fixed existing packed inputs, seed20261103 paired order. Wall timer covers kernel; perf counts whole process including one warm-up/load. All pairs retained. No concurrent agent build/benchmark.','baseline_sha256':hashlib.sha256(baseline.read_bytes()).hexdigest(),'candidate_sha256':hashlib.sha256(a.candidate.read_bytes()).hexdigest(),'runs':rows,'summary':summary};(a.output/'summary.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(summary,indent=2))
