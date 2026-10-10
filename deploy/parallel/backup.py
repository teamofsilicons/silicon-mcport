#!/usr/bin/env python3
"""Back up only one isolated Accounts app; preserve its current encryption keys."""
import argparse,datetime,hashlib,json,os,pathlib,shutil,sqlite3,subprocess,tarfile,tempfile
p=argparse.ArgumentParser();p.add_argument('app',choices=['mcport']);a=p.parse_args();app=a.app;os.umask(0o077)
root=pathlib.Path('/var/backups/'+app+'-accounts');root.mkdir(mode=0o700,parents=True,exist_ok=True);root.chmod(0o700);stamp=datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')
with tempfile.TemporaryDirectory(prefix=stamp+'-',dir=root) as name:
 d=pathlib.Path(name);config=pathlib.Path('/etc/'+app+'-accounts');data=pathlib.Path('/var/lib/'+app+'-accounts')
 shutil.copytree(config,d/'config')
 if app=='waveform':
  with (d/'database.dump').open('wb') as f:subprocess.run(['docker','exec','waveform-accounts-postgres','pg_dump','-U','postgres','-d','waveform_accounts','-Fc'],check=True,stdout=f,stderr=subprocess.PIPE)
  assert (d/'database.dump').read_bytes()[:5]==b'PGDMP'
 else:
  with sqlite3.connect((data/'mcport.sqlite').as_uri()+'?mode=ro',uri=True) as src:
   with sqlite3.connect(d/'mcport.sqlite') as dst:src.backup(dst);assert dst.execute('PRAGMA integrity_check').fetchone()==('ok',)
  shutil.copyfile(data/'master.key',d/'master.key');assert (d/'master.key').stat().st_size==32
 for unit in pathlib.Path('/etc/systemd/system').glob(app+'-accounts*.service'):shutil.copyfile(unit,d/unit.name)
 manifest={'app':app,'created_at':stamp,'release':str(pathlib.Path('/opt/'+app+'-accounts/current').resolve()),'files':{str(f.relative_to(d)):{'bytes':f.stat().st_size,'sha256':hashlib.sha256(f.read_bytes()).hexdigest()} for f in d.rglob('*') if f.is_file()}}
 (d/'manifest.json').write_text(json.dumps(manifest,indent=2));archive=root/(stamp+'.tar.gz')
 with tarfile.open(archive,'w:gz') as t:
  for f in d.rglob('*'):
   if f.is_file():t.add(f,arcname=str(f.relative_to(d)),recursive=False)
 archive.chmod(0o600)
 uri='s3://silicon-hook-standalone-artifacts-lxpfsbc0jpuk/parallel-accounts-20261010/'+app+'/backups/'+archive.name
 subprocess.run(['aws','s3','cp',str(archive),uri,'--sse','AES256','--region','us-east-1','--only-show-errors'],check=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
 report={'app':app,'backup_uri':uri,'archive_sha256':hashlib.sha256(archive.read_bytes()).hexdigest(),'files':len(manifest['files']),'key_preserved':True,'database_snapshot_verified':True}
 (root/'latest.json').write_text(json.dumps(report,indent=2));print(json.dumps(report))
