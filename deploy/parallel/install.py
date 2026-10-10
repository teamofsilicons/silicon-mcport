#!/usr/bin/env python3
import argparse,base64,tempfile,hashlib,importlib.util,json,os,pathlib,re,shlex,subprocess,time,urllib.request,urllib.parse,urllib.error
p=argparse.ArgumentParser();p.add_argument('app',choices=['mcport']);p.add_argument('--archive',type=pathlib.Path,required=True);p.add_argument('--sha256',required=True);p.add_argument('--revision',required=True);p.add_argument('--validator',type=pathlib.Path,required=True);p.add_argument('--resume-staged',action='store_true');a=p.parse_args();app=a.app
os.umask(0o077);assert re.fullmatch('[0-9a-f]{40}',a.revision) and hashlib.sha256(a.archive.read_bytes()).hexdigest()==a.sha256
prefix=pathlib.Path('/opt/'+app+'-accounts');config=pathlib.Path('/etc/'+app+'-accounts');release=prefix/'releases'/a.revision
assert config.is_dir()
if a.resume_staged:assert release.is_dir() and (prefix/'current').resolve()==release
else:assert not release.exists() and not (prefix/'current').exists()
legacy='waveform-api' if app=='waveform' else 'mcport'
def run(args,**kw):return subprocess.run(args,check=True,capture_output=True,text=True,**kw).stdout
before=run(['systemctl','show',legacy,'--property=MainPID,ActiveState']);assert 'ActiveState=active' in before
v=dict(x.split('=',1) for x in shlex.split((config/'runtime.env').read_text()))
class NoRedirect(urllib.request.HTTPRedirectHandler):
 def redirect_request(self,*args):return None
opener=urllib.request.build_opener(NoRedirect());basic=base64.b64encode((app+':'+v[app.upper()+'_APP_SECRET']).encode()).decode()
request=urllib.request.Request('https://accounts.teamofsilicons.com/v1/oauth/introspect',data=urllib.parse.urlencode({'token':app+'-parallel-deploy-preflight'}).encode(),headers={'Authorization':'Basic '+basic,'Content-Type':'application/x-www-form-urlencoded'})
with opener.open(request,timeout=25) as r:assert json.load(r)=={'active':False}
s=importlib.util.spec_from_file_location('validated_installer',a.validator);m=importlib.util.module_from_spec(s);s.loader.exec_module(m)
if app=='waveform':
 if a.resume_staged:
  with tempfile.TemporaryDirectory(prefix='waveform-verify-') as temp:
   verified=pathlib.Path(temp)/'release';m.stage(a.archive,verified,a.revision)
   assert {str(x.relative_to(verified)):hashlib.sha256(x.read_bytes()).hexdigest() for x in verified.rglob('*') if x.is_file()}=={str(x.relative_to(release)):hashlib.sha256(x.read_bytes()).hexdigest() for x in release.rglob('*') if x.is_file()}
 else:m.stage(a.archive,release,a.revision)
 binary=release/'bin/waveform-api';health='http://172.18.0.1:8180/health/ready'
else:
 meta,members=m.validate_bundle(a.archive,a.sha256,a.revision)
 if a.resume_staged:
  assert set(x.name for x in release.iterdir())==set(members)
  assert all((release/name).read_bytes()==content for name,(content,mode) in members.items())
 else:
  release.mkdir(mode=0o755);release.chmod(0o755)
  for name,(content,mode) in members.items():f=release/name;f.write_bytes(content);f.chmod(mode)
 binary=release/'mcport-server';health='http://127.0.0.1:4381/health'
if not a.resume_staged:(prefix/'current').symlink_to(release)
run(['systemctl','enable','--now',app+'-accounts'])
ready=False
for _ in range(60):
 try:
  with urllib.request.urlopen(health,timeout=3) as r:
   if r.status==200:ready=True;body=json.load(r);break
 except (urllib.error.URLError,TimeoutError):pass
 time.sleep(1)
if not ready:raise RuntimeError('New API readiness failed; publicmaintenance retained; inspect new unit only')
if app=='mcport':assert body['source_revision']==a.revision
proxy=pathlib.Path('/etc/waveform/Caddyfile' if app=='waveform' else '/etc/caddy/Caddyfile');previous=proxy.read_text();gate='api.'+app+'.teamofsilicons.com {\n    header Retry-After 120\n    respond "Accounts app rollout in progress" 503\n}'
assert previous.count(gate)==1
upstream='172.18.0.1:8180' if app=='waveform' else '127.0.0.1:4381'
block='api.'+app+'.teamofsilicons.com {\n    encode zstd gzip\n    header X-Content-Type-Options nosniff\n    reverse_proxy '+upstream+'\n}'
proxy.write_text(previous.replace(gate,block));oldprefix=proxy.with_name('Caddyfile.before-accounts-20261010').read_bytes();assert proxy.read_bytes().startswith(oldprefix)
try:
 if app=='waveform':run(['docker','exec','waveform-proxy','caddy','validate','--config','/etc/caddy/Caddyfile']);run(['docker','exec','waveform-proxy','caddy','reload','--config','/etc/caddy/Caddyfile'])
 else:run(['caddy','validate','--config',str(proxy)]);run(['systemctl','reload','caddy'])
except BaseException:proxy.write_text(previous);raise
assert run(['systemctl','show',legacy,'--property=MainPID,ActiveState'])==before
report={'app':app,'source_revision':a.revision,'archive_sha256':a.sha256,'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'new_api_ready':True,'app_credential_preflight':True,'legacy_pid_unchanged':True,'legacy_proxy_prefix_preserved':True,'public_url':'https://api.'+app+'.teamofsilicons.com','isolated_data':'/var/lib/'+app+'-accounts','health':body}
(config/'release.json').write_text(json.dumps(report,indent=2));print(json.dumps(report))
