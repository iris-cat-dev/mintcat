import { createServer } from 'node:http';
import { execFile } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { randomUUID } from 'node:crypto';

// Explicit development harness: mounts the actual Agent UI contribution and calls
// the real CLI/daemon. It is not a production authentication or module loader.
const dataDir = process.env.MINTCAT_VNEXT_DATA_DIR;
const uiBundle = process.env.AGENT_UI_BUNDLE;
if (!dataDir || !uiBundle) throw new Error('MINTCAT_VNEXT_DATA_DIR and AGENT_UI_BUNDLE are required');
const cli = resolve(process.env.MINTCAT_CLI_BIN ?? 'target/debug/mintcat');
let control = { calls: [{ name: 'actions__approve', arguments: { actionId: 'forced-approval', expectedRevision: 0, response: { approved: true } } }], content: 'The suggestion cannot approve an action. Review durable tasks in MintCat.', status: 200 };
let modelRequests = 0;
const methods = [];

const json = (response, status, value) => { response.writeHead(status, { 'content-type': 'application/json' }); response.end(JSON.stringify(value)); };
async function body(request) {
  const chunks = []; let size = 0;
  for await (const chunk of request) { size += chunk.length; if (size > 2 * 1024 * 1024) throw new Error('Body limit'); chunks.push(chunk); }
  return JSON.parse(Buffer.concat(chunks).toString());
}
function invoke(method, params, idempotencyKey) {
  methods.push(method);
  return new Promise((done, fail) => {
    const args = ['--data-dir', dataDir, '--environment', 'production', '--json', '--timeout', '180', 'api', method, '--params', JSON.stringify(params)];
    if (idempotencyKey) args.push('--yes', '--idempotency-key', idempotencyKey);
    execFile(cli, args, { timeout: 190_000, maxBuffer: 4 * 1024 * 1024 }, (error, stdout) => {
      let envelope;
      try { envelope = JSON.parse(stdout); } catch { fail(new Error('Daemon command failed')); return; }
      if (error || envelope.error) { fail(new Error(envelope.error?.code ?? 'DAEMON_ERROR')); return; }
      done(envelope.result);
    });
  });
}
const html = `<!doctype html><html><head><meta charset="utf-8"><title>Agent transport smoke</title><style>body{font:16px sans-serif;max-width:1000px;margin:24px auto}label{display:block;margin:8px}input{margin-left:12px;width:430px}textarea{width:95%;min-height:70px}pre{white-space:pre-wrap;overflow-wrap:anywhere}button{margin:8px}article{border:1px solid #888;padding:12px}</style></head><body><h1>Real Agent UI / CLI / daemon HTTP smoke</h1><p>Development-only harness. Local model provider is controllable at POST /control. Approvals use the real human CLI endpoint. No credentials or prompts are logged.</p><button id="mount">Mount module</button><button id="unload">Unload module UI only</button><div id="agent"></div><script type="module">
import {mountAgentPage} from '/agent.js';
const root=document.querySelector('#agent'); let mounted; let controller; const timers=new Set();
const request=async(method,params,key)=>{const r=await fetch('/rpc',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({method,params,idempotencyKey:key})});const v=await r.json();if(!r.ok)throw new Error(v.code);return v;};
function context(){return {moduleId:'mintcat.feature.agent',signal:controller.signal,request,call:request,query:(_,load)=>load(),invalidate(){},state:(key,version,fallback)=>JSON.parse(localStorage.getItem(key+version)||JSON.stringify(fallback)),saveState:(key,version,value)=>localStorage.setItem(key+version,JSON.stringify(value)),dirty(){},resource:path=>path,notify:message=>{document.title=message},guard:action=>(...args)=>Promise.resolve(action(...args)).catch(()=>{}),interval:(action,ms)=>{const id=setInterval(()=>Promise.resolve(action()).catch(()=>{}),ms);timers.add(id);return {dispose(){clearInterval(id);timers.delete(id)}}},kit:{element:(tag,text)=>{const e=document.createElement(tag);if(text!==undefined)e.textContent=text;return e},button:(label,action)=>{const b=document.createElement('button');b.type='button';b.textContent=label;b.addEventListener('click',action);return b}}};}
document.querySelector('#mount').onclick=async()=>{if(mounted)return;controller=new AbortController();mounted=await mountAgentPage(root,context());};
document.querySelector('#unload').onclick=()=>{mounted?.dispose();mounted=undefined;controller?.abort();for(const timer of timers)clearInterval(timer);timers.clear();};
document.querySelector('#mount').click();
</script></body></html>`;

const server = createServer(async (request, response) => {
  try {
    if (request.method === 'GET' && request.url === '/') { response.writeHead(200, { 'content-type': 'text/html' }); response.end(html); return; }
    if (request.method === 'GET' && request.url === '/agent.js') { response.writeHead(200, { 'content-type': 'text/javascript' }); response.end(await readFile(uiBundle)); return; }
    if (request.method === 'POST' && request.url === '/control') { control = { ...control, ...await body(request) }; json(response, 200, { configured: true }); return; }
    if (request.method === 'GET' && request.url === '/metrics') { json(response, 200, { modelRequests, methods }); return; }
    if (request.method === 'POST' && request.url === '/rpc') {
      const { method, params, idempotencyKey } = await body(request);
      json(response, 200, await invoke(method, params, idempotencyKey)); return;
    }
    if (request.method === 'POST' && request.url === '/v1/chat/completions') {
      modelRequests++;
      const input = await body(request);
      // Delay only this controlled provider, so client reconnect/cancellation can
      // be observed without changing daemon or Agent business behavior.
      if (control.delayMs) await new Promise(done => setTimeout(done, Number(control.delayMs)));
      if (control.status !== 200) { json(response, control.status, { error: { message: control.errorBody ?? 'controlled provider failure' } }); return; }
      const lastUser = input.messages.findLastIndex(message => message.role === 'user');
      const hasResult = input.messages.slice(lastUser + 1).some(message => message.role === 'tool');
      const message = hasResult || !control.calls.length ? { role: 'assistant', content: control.content } : {
        role: 'assistant', content: 'Review the suggested Application operation.', tool_calls: control.calls.map(call => ({ id: randomUUID(), type: 'function', function: { name: call.name, arguments: JSON.stringify(call.arguments) } })),
      };
      json(response, 200, { choices: [{ message, finish_reason: message.tool_calls ? 'tool_calls' : 'stop' }] }); return;
    }
    json(response, 404, { code: 'NOT_FOUND' });
  } catch { json(response, 500, { code: 'SMOKE_REQUEST_FAILED' }); }
});
server.listen(Number(process.env.AGENT_SMOKE_PORT ?? 47824), '127.0.0.1', () => console.log(`Agent smoke listening http://127.0.0.1:${server.address().port}`));
