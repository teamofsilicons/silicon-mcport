#!/usr/bin/env python3
"""Execute an existing candidate archive on a matching native CI runner."""
import argparse,hashlib,json,os,pathlib,platform,re,subprocess,tarfile,tempfile
p=argparse.ArgumentParser();p.add_argument('--app',choices=['waveform','mcport'],required=True);p.add_argument('--archive',type=pathlib.Path,required=True);p.add_argument('--target',required=True);p.add_argument('--source-revision',required=True);p.add_argument('--candidate-run',required=True);p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args()
assert re.fullmatch('[0-9a-f]{40}',a.source_revision) and a.candidate_run.isdigit()
system={'Linux':'linux','Darwin':'macos','Windows':'windows'}[platform.system()]
assert a.target.startswith(system+'-')
arch={'x86_64':'x86_64','amd64':'x86_64','arm64':'aarch64','aarch64':'aarch64'}[platform.machine().lower()]
assert a.target==system+'-'+arch,'Native proof must run on the target architecture'
name=a.app+('.exe' if system=='windows' else '')
with tempfile.TemporaryDirectory(prefix=a.app+'-catalog-proof-') as tmp:
 root=pathlib.Path(tmp).resolve();home=root/'home';home.mkdir();binary=root/name
 with tarfile.open(a.archive,'r:gz') as arc:
  members={m.name.removeprefix('./'):m for m in arc.getmembers()}
  assert set(members)=={'apps.yaml','bin/'+name} and all(m.isfile() for m in members.values())
  manifest=arc.extractfile(members['apps.yaml']).read().decode();raw=arc.extractfile(members['bin/'+name]).read()
  assert re.search(r'^app_id: '+a.app+r'$',manifest,re.M) and re.search(r'^  '+a.target+r':$',manifest,re.M)
  version=re.search(r'^version: (\d+\.\d+\.\d+)$',manifest,re.M).group(1)
 binary.write_bytes(raw);binary.chmod(0o755)
 env={'HOME':str(home),'USERPROFILE':str(home),'SILICON_HOME':str(home),'NO_COLOR':'1','PATH':os.pathsep.join(['/usr/bin','/bin'])}
 if system=='windows':
  sr=os.environ.get('SYSTEMROOT',r'C:\Windows');env.update({'SYSTEMROOT':sr,'WINDIR':sr,'PATH':os.pathsep.join([sr,os.path.join(sr,'System32')])})
 commands=[]
 for args in [['--version'],['--help'],['accounts','--json'],['login','status','--json']]:
  r=subprocess.run([str(binary),*args],env=env,cwd=home,capture_output=True,text=True,timeout=60)
  commands.append({'args':args,'exit_code':r.returncode,'stdout':r.stdout,'stderr':r.stderr});assert r.returncode==0
 assert commands[0]['stdout'].strip()==a.app+' '+version and commands[1]['stdout'].strip()
 assert json.loads(commands[2]['stdout'])['app_id']==a.app
 assert json.loads(commands[3]['stdout'])['authenticated'] is False
 assert not list(home.rglob('*')),'Discovery unexpectedly created state'
 result={'schema_version':1,'app_id':a.app,'version':version,'target':a.target,'source_revision':a.source_revision,'candidate_run':a.candidate_run,'archive_sha256':hashlib.sha256(a.archive.read_bytes()).hexdigest(),'binary_sha256':hashlib.sha256(raw).hexdigest(),'runner_os':platform.system(),'runner_machine':platform.machine(),'native':True,'commands':commands,'empty_home_unchanged':True}
 a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(json.dumps(result,indent=2)+'\n');print(json.dumps({k:v for k,v in result.items() if k!='commands'}))
