#!/usr/bin/env python3
"""A loopback-only desktop MCP used for manual remote-asset verification."""
import argparse
import base64
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SVG = b'<svg xmlns="http://www.w3.org/2000/svg" width="120" height="80"><rect width="120" height="80" fill="#315b43"/><text x="12" y="44" fill="white">MCPort asset</text></svg>'
PNG = base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScLbtAAAAABJRU5ErkJggg==')
TOKEN = 'fixture-asset-account'

class Handler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self,*_): pass
    def reply(self,status,value=None,mime='application/json',headers=None):
        raw=b'' if value is None else json.dumps(value).encode() if mime=='application/json' else value
        self.send_response(status);self.send_header('Content-Type',mime);self.send_header('Content-Length',str(len(raw)))
        for key,value in (headers or {}).items():self.send_header(key,value)
        self.end_headers();self.wfile.write(raw)
    def record(self):
        with self.server.lock:self.server.requests.append({'method':self.command,'path':self.path,'authorized':self.headers.get('Authorization')=='Bearer '+TOKEN})
    def do_GET(self):
        if self.path=='/health':return self.reply(200,{'status':'ok'})
        if self.path=='/proof':return self.reply(200,{'requests':self.server.requests})
        self.record()
        if self.headers.get('Authorization')!='Bearer '+TOKEN:return self.reply(401,{'error':'fixture account required'})
        if self.path=='/assets/card.svg':return self.reply(200,SVG,'image/svg+xml')
        if self.path=='/assets/pixel.png':return self.reply(200,PNG,'image/png')
        if self.path=='/assets/redirect.svg':return self.reply(302,b'','text/plain',{'Location':'/private/secret.txt'})
        if self.path=='/private/secret.txt':return self.reply(200,b'THIS MUST NEVER BE FETCHED','text/plain')
        return self.reply(404,{'error':'not found'})
    def do_DELETE(self):self.reply(204)
    def do_POST(self):
        self.record()
        body=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))) or b'{}')
        if self.headers.get('Authorization')!='Bearer '+TOKEN:return self.reply(401,{'error':'fixture account required'})
        method=body.get('method');origin=self.server.origin
        if 'id' not in body:return self.reply(202)
        if method=='initialize':result={'protocolVersion':body.get('params',{}).get('protocolVersion','2025-11-25'),'capabilities':{'tools':{},'resources':{}},'serverInfo':{'name':'desktop-asset-proof','version':'1.0'}}
        elif method=='tools/list':result={'tools':[{'name':'design_asset','description':'Return local desktop SVG and PNG assets','inputSchema':{'type':'object','properties':{},'additionalProperties':False}}]}
        elif method=='tools/call':result={'content':[{'type':'text','text':f'Use [design]({origin}/assets/card.svg) and {origin}/assets/pixel.png. Rejected redirect: {origin}/assets/redirect.svg. File links are not host capabilities: file:///etc/passwd'},{'type':'resource_link','name':'design-file','uri':origin+'/assets/card.svg','mimeType':'image/svg+xml'}],'structuredContent':{'account':'fixture-shared-asset-account','source':'local desktop'},'isError':False}
        elif method=='resources/list':result={'resources':[{'uri':'fixture://readme','name':'Asset README','mimeType':'text/plain'}]}
        elif method=='resources/read':result={'contents':[{'uri':body.get('params',{}).get('uri'),'mimeType':'text/plain','text':'Explicit MCP resource read works through the same host.'}]}
        else:return self.reply(200,{'jsonrpc':'2.0','id':body['id'],'error':{'code':-32601,'message':'unsupported fixture method'}})
        self.reply(200,{'jsonrpc':'2.0','id':body['id'],'result':result})

def main():
    p=argparse.ArgumentParser();p.add_argument('--port',type=int,default=4392);args=p.parse_args()
    server=ThreadingHTTPServer(('127.0.0.1',args.port),Handler);server.origin=f'http://127.0.0.1:{args.port}';server.requests=[];server.lock=threading.Lock()
    print('Asset MCP listening at '+server.origin+'/mcp',flush=True)
    try:server.serve_forever()
    except KeyboardInterrupt:pass
    finally:server.server_close()
if __name__=='__main__':main()
