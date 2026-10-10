#!/usr/bin/env python3
"""Create isolated Accounts runtime stores; no legacy writers, data or keys change."""
import argparse,hashlib,json,os,pathlib,pwd,secrets,shlex,socket,subprocess,time,urllib.parse
p=argparse.ArgumentParser();p.add_argument('app',choices=['mcport']);a=p.parse_args();app=a.app
os.umask(0o077)
def run(args,**kw):
 r=subprocess.run(args,capture_output=True,text=True,**kw)
 if r.returncode:raise RuntimeError('Stage failed: '+args[0]+' '+str(r.returncode))
 return r.stdout
def write(path,text,mode=0o600):path=pathlib.Path(path);path.write_text(text);path.chmod(mode)
def envtext(v):
 assert all(not any(c in str(x) for c in '\n\r\x00') for x in v.values())
 return ''.join(k+'='+json.dumps(str(x))+'\n' for k,x in v.items())
def envread(path):return dict(x.split('=',1) for x in shlex.split(pathlib.Path(path).read_text(),comments=True))
config=pathlib.Path('/etc/'+app+'-accounts');data=pathlib.Path('/var/lib/'+app+'-accounts');prefix=pathlib.Path('/opt/'+app+'-accounts')
for d in [config,data,prefix]:assert not d.exists(),'Independent namespace exists; inspect before retrying'
legacy_unit='waveform-api' if app=='waveform' else 'mcport'
before=run(['systemctl','show',legacy_unit,'--property=MainPID,ActiveState'])
assert 'ActiveState=active' in before
port=8180 if app=='waveform' else 4381;bind='172.18.0.1' if app=='waveform' else '127.0.0.1'
with socket.socket() as s:s.bind((bind,port))
name='silicon-'+app+'/accounts-production/runtime';secret=json.loads(json.loads(run(['aws','--region','us-east-1','secretsmanager','get-secret-value','--secret-id',name]))['SecretString'])
assert secret['app_secret'].startswith('sa_app_') and secret['webhook_secret'].startswith('whsec_')
for d in [config,prefix]:d.mkdir(mode=0o755);d.chmod(0o755)
data.mkdir(mode=0o700);(prefix/'releases').mkdir(mode=0o755);(prefix/'releases').chmod(0o755)
user=app+'-accounts'
try:pwd.getpwnam(user)
except KeyError:run(['useradd','--system','--home-dir',str(data),'--shell','/sbin/nologin',user])
u=pwd.getpwnam(user);os.chown(data,u.pw_uid,u.pw_gid)
env={'ACCOUNTS_URL':'https://accounts.teamofsilicons.com'}
web={'NODE_ENV':'production','APP_ID':app,'APP_SECRET':secret['app_secret'],'SESSION_SECRET':secret['session_secret'],'ACCOUNTS_URL':env['ACCOUNTS_URL'],'APP_API_URL':'https://api.'+app+'.teamofsilicons.com','PUBLIC_URL':'https://'+app+'.teamofsilicons.com'}
if app=='mcport':
 key=bytes.fromhex(secret['master_key']);assert len(key)==32
 (data/'master.key').write_bytes(key);(data/'master.key').chmod(0o600);os.chown(data/'master.key',u.pw_uid,u.pw_gid)
 env.update({'MCPORT_APP_ID':app,'MCPORT_APP_SECRET':secret['app_secret'],'MCPORT_ACCOUNTS_WEBHOOK_SECRET':secret['webhook_secret'],'MCPORT_BIND':bind+':'+str(port),'MCPORT_PUBLIC_URL':web['APP_API_URL'],'MCPORT_WEB_URL':web['PUBLIC_URL'],'MCPORT_DATA_DIR':str(data)})
 command=str(prefix/'current/mcport-server');requires=''
else:
 old=envread('/etc/waveform/native.env');providers=('WAVEFORM_GEMINI_','WAVEFORM_ELEVENLABS_','WAVEFORM_OPENAI_','WAVEFORM_DEEPGRAM_')
 env.update({k:v for k,v in old.items() if k.startswith(providers)})
 assert all(env.get('WAVEFORM_'+x+'_API_KEY') for x in ['GEMINI','ELEVENLABS','OPENAI','DEEPGRAM'])
 env.update({'WAVEFORM_ENVIRONMENT':'production','WAVEFORM_BIND_ADDR':bind+':'+str(port),'WAVEFORM_APP_ID':app,'WAVEFORM_APP_SECRET':secret['app_secret'],'WAVEFORM_ACCOUNTS_WEBHOOK_SECRET':secret['webhook_secret'],'WAVEFORM_ENCRYPTION_KEY':secret['encryption_key'],'WAVEFORM_IDEMPOTENCY_DIGEST_KEY':secret['idempotency_digest_key'],'WAVEFORM_PUBLIC_URL':web['APP_API_URL'],'WAVEFORM_BRIEFCASE_BASE_URL':'https://api.briefcase.teamofsilicons.com','WAVEFORM_BRIEFCASE_PERMANENT_ORIGIN':'https://briefcase.teamofsilicons.com','WAVEFORM_BRIEFCASE_APP_ID':'briefcase','WAVEFORM_BRIEFCASE_APP_FOLDER':'apps/waveform','WAVEFORM_REQUIRE_FULL_PROVIDER_CHAIN':'true','WAVEFORM_LOG_JSON':'true','WAVEFORM_MAX_IN_FLIGHT':'8','WAVEFORM_DATABASE_MAX_CONNECTIONS':'8','WAVEFORM_DATABASE_MIN_CONNECTIONS':'1','WAVEFORM_FFMPEG_PATH':str(prefix/'current/bin/ffmpeg'),'SPACE_STATION_HOME':str(data/'telemetry'),'WAVEFORM_SPEECH_FIXTURES':'false'})
 for provider in providers:env[provider+'MAX_CONCURRENCY']='2'
 tls=config/'postgres-tls';tls.mkdir(mode=0o755);tls.chmod(0o755)
 run(['openssl','req','-x509','-newkey','rsa:3072','-nodes','-days','3650','-subj','/CN=Waveform Accounts PostgreSQL CA','-keyout',str(tls/'ca.key'),'-out',str(tls/'ca.crt')])
 run(['openssl','req','-newkey','rsa:3072','-nodes','-subj','/CN=127.0.0.1','-keyout',str(tls/'server.key'),'-out',str(tls/'server.csr')])
 write(tls/'extensions.cnf','subjectAltName=IP:127.0.0.1\nextendedKeyUsage=serverAuth\n')
 run(['openssl','x509','-req','-days','825','-in',str(tls/'server.csr'),'-CA',str(tls/'ca.crt'),'-CAkey',str(tls/'ca.key'),'-CAcreateserial','-extfile',str(tls/'extensions.cnf'),'-out',str(tls/'server.crt')])
 for f in ['server.key','server.crt']:os.chown(tls/f,999,999)
 for f in ['server.crt','ca.crt']:(tls/f).chmod(0o644)
 (tls/'server.key').chmod(0o600)
 write(config/'postgres.env','POSTGRES_DB=waveform_accounts\nPOSTGRES_USER=postgres\nPOSTGRES_PASSWORD='+secrets.token_hex(32)+'\nPOSTGRES_INITDB_ARGS=--auth-host=scram-sha-256\n')
 password=secret['postgres_password'];assert len(password)==64 and all(c in '0123456789abcdef' for c in password)
 write(config/'init.sql',"CREATE ROLE waveform_accounts LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD '"+password+"';\nALTER DATABASE waveform_accounts OWNER TO waveform_accounts;\nGRANT ALL ON SCHEMA public TO waveform_accounts;\n");os.chown(config/'init.sql',999,999)
 write(config/'pg_hba.conf','local all all trust\nhostssl all all 0.0.0.0/0 scram-sha-256\nhostnossl all all 0.0.0.0/0 reject\n',0o644)
 pg=data/'postgres';pg.mkdir(mode=0o700);os.chown(pg,999,999)
 image=json.loads(run(['docker','inspect','waveform-postgres']))[0]['Image'];assert image.startswith('sha256:')
 args=['/usr/bin/docker','run','--name','waveform-accounts-postgres','--memory','640m','--security-opt','no-new-privileges','--log-opt','max-size=10m','--log-opt','max-file=3','-p','127.0.0.1:5434:5432','--env-file',str(config/'postgres.env'),'-v',str(pg)+':/var/lib/postgresql/data','-v',str(config/'init.sql')+':/docker-entrypoint-initdb.d/001-app.sql:ro','-v',str(tls/'server.key')+':/etc/postgres-tls/server.key:ro','-v',str(tls/'server.crt')+':/etc/postgres-tls/server.crt:ro','-v',str(config/'pg_hba.conf')+':/etc/postgres-hba.conf:ro',image,'-c','ssl=on','-c','ssl_cert_file=/etc/postgres-tls/server.crt','-c','ssl_key_file=/etc/postgres-tls/server.key','-c','hba_file=/etc/postgres-hba.conf','-c','shared_buffers=64MB','-c','max_connections=40']
 write('/etc/systemd/system/waveform-accounts-postgres.service','[Unit]\nDescription=Waveform Accounts isolated PostgreSQL\nRequires=docker.service\nAfter=docker.service network-online.target\n[Service]\nRestart=always\nRestartSec=5\nTimeoutStartSec=240\nExecStartPre=-/usr/bin/docker rm waveform-accounts-postgres\nExecStart='+shlex.join(args)+'\nExecStop=/usr/bin/docker stop --time 30 waveform-accounts-postgres\n[Install]\nWantedBy=multi-user.target\n',0o644)
 env['WAVEFORM_DATABASE_URL']='postgres://waveform_accounts:'+password+'@127.0.0.1:5434/waveform_accounts?sslmode=verify-full&sslrootcert='+str(tls/'ca.crt')
 command=str(prefix/'current/bin/waveform-api');requires='Requires=waveform-accounts-postgres.service\nAfter=waveform-accounts-postgres.service\n'
write(config/'runtime.env',envtext(env));write(config/'web.env',envtext(web))
unit='[Unit]\nDescription='+app+' isolated Accounts API\nAfter=network-online.target\n'+requires+'[Service]\nType=simple\nUser='+user+'\nGroup='+user+'\nEnvironmentFile='+str(config/'runtime.env')+'\nExecStart='+command+'\nWorkingDirectory='+str(data)+'\nRestart=on-failure\nRestartSec=5\nTimeoutStopSec=60\nNoNewPrivileges=true\nPrivateTmp=true\nProtectSystem=strict\nProtectHome=true\nProtectKernelTunables=true\nProtectKernelModules=true\nProtectControlGroups=true\nRestrictSUIDSGID=true\nUMask=0077\nMemoryMax=768M\nTasksMax=256\nReadWritePaths='+str(data)+'\n[Install]\nWantedBy=multi-user.target\n'
write('/etc/systemd/system/'+app+'-accounts.service',unit,0o644)
run(['systemctl','daemon-reload'])
if app=='waveform':
 run(['systemctl','enable','--now','waveform-accounts-postgres'])
 for _ in range(60):
  if subprocess.run(['docker','exec','waveform-accounts-postgres','pg_isready','-U','postgres'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode==0:break
  time.sleep(1)
 else:raise RuntimeError('New PostgreSQL did not become ready')
configproxy=pathlib.Path('/etc/waveform/Caddyfile' if app=='waveform' else '/etc/caddy/Caddyfile');oldproxy=configproxy.read_bytes();domain='api.'+app+'.teamofsilicons.com'
assert domain.encode() not in oldproxy
backup=configproxy.with_name('Caddyfile.before-accounts-20261010');assert not backup.exists();backup.write_bytes(oldproxy);backup.chmod(0o600)
configproxy.write_bytes(oldproxy+('\n# Independent Accounts service\n'+domain+' {\n    header Retry-After 120\n    respond "Accounts app rollout in progress" 503\n}\n').encode())
try:
 if app=='waveform':
  run(['docker','exec','waveform-proxy','caddy','validate','--config','/etc/caddy/Caddyfile','--adapter','caddyfile']);run(['docker','exec','waveform-proxy','caddy','reload','--config','/etc/caddy/Caddyfile','--adapter','caddyfile'])
 else:
  run(['caddy','validate','--config',str(configproxy)]);run(['systemctl','reload','caddy'])
except BaseException:configproxy.write_bytes(oldproxy);raise
assert run(['systemctl','show',legacy_unit,'--property=MainPID,ActiveState'])==before,'Legacy writer changed unexpectedly'
print(json.dumps({'app':app,'runtime_ready':True,'api_not_started':True,'fresh_storage':str(data),'new_service':app+'-accounts','new_public_status':503,'legacy_writer_unchanged':True,'legacy_proxy_prefix_preserved':configproxy.read_bytes().startswith(oldproxy),'legacy_proxy_sha256':hashlib.sha256(oldproxy).hexdigest()}))
