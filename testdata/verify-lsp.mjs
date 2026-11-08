// End-to-end smoke test for the Rust LSP server over real stdio JSON-RPC.
// Usage: cargo build --workspace && node testdata/verify-lsp.mjs
import { spawn } from 'node:child_process';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..');
const exe =
  process.env.LD_JAVA_ANALYZE_SERVER ??
  path.join(
    root,
    'target',
    'debug',
    process.platform === 'win32' ? 'ld-java-analyze.exe' : 'ld-java-analyze',
  );
const uri = (p) => `file:///${p.replace(/\\/g, '/')}`;

const server = spawn(exe, [], { stdio: ['pipe', 'pipe', 'pipe'] });
let buffer = Buffer.alloc(0);
let nextId = 0;
const pending = new Map();
let stderr = '';
server.stderr.on('data', (chunk) => { stderr += chunk.toString(); });

server.stdout.on('data', (chunk) => {
  buffer = Buffer.concat([buffer, chunk]);
  for (;;) {
    const split = buffer.indexOf('\r\n\r\n');
    if (split < 0) return;
    const match = /Content-Length:\s*(\d+)/i.exec(buffer.subarray(0, split).toString());
    if (!match) return;
    const length = Number(match[1]);
    if (buffer.length < split + 4 + length) return;
    const body = buffer.subarray(split + 4, split + 4 + length).toString();
    buffer = buffer.subarray(split + 4 + length);
    try {
      const message = JSON.parse(body);
      if (message.id !== undefined && pending.has(message.id)) {
        pending.get(message.id)(message);
        pending.delete(message.id);
      }
    } catch { /* ignore non-JSON notifications */ }
  }
});

function send(method, params, withId = true) {
  const payload = { jsonrpc: '2.0', method, params };
  let promise = Promise.resolve(null);
  if (withId) {
    const id = ++nextId;
    payload.id = id;
    promise = new Promise((resolve) => pending.set(id, resolve));
  }
  const text = JSON.stringify(payload);
  server.stdin.write(`Content-Length: ${Buffer.byteLength(text)}\r\n\r\n${text}`);
  return promise;
}

const result = (r) => (r?.error ? `ERROR ${JSON.stringify(r.error)}` : JSON.stringify(r?.result));
const base = path.join(root, 'testdata', 'demo1');
const controller = uri(path.join(base, 'src/main/java/demo/UserController.java'));
const service = uri(path.join(base, 'src/main/java/demo/UserService.java'));
const mapperJava = uri(path.join(base, 'src/main/java/demo/UserMapper.java'));
const mapperXml = uri(path.join(base, 'src/main/resources/mapper/UserMapper.xml'));
const pos = (line, character) => ({ line, character });

const init = await send('initialize', {
  processId: process.pid,
  capabilities: {},
  rootUri: uri(root),
  workspaceFolders: [{ uri: uri(root), name: 'ld_java_analyze' }],
  initializationOptions: {
    referenceRoots: [],
    excludeRules: ['generated/', 'file:*.generated.java'],
    useBuiltinExcludes: true,
  },
});
if (init?.error) {
  console.error('initialize failed:', result(init));
  process.exit(1);
}
console.log('initialize: ok');
await send('initialized', {}, false);
console.log('stats:', result(await send('workspace/executeCommand', {
  command: 'ld-java-analyze.stats', arguments: [{}],
})));

await send('textDocument/didOpen', {
  textDocument: {
    uri: controller,
    languageId: 'java',
    version: 1,
    text: readFileSync(path.join(base, 'src/main/java/demo/UserController.java'), 'utf8'),
  },
}, false);

const cases = [
  ['definition(findById)', 'textDocument/definition', { textDocument: { uri: controller }, position: pos(7, 30) }],
  ['definition(getName @Lombok)', 'textDocument/definition', { textDocument: { uri: controller }, position: pos(9, 22) }],
  ['references(findById)', 'textDocument/references', { textDocument: { uri: controller }, position: pos(7, 30), context: { includeDeclaration: true } }],
  ['implementation(interface)', 'textDocument/implementation', { textDocument: { uri: service }, position: pos(3, 12) }],
  ['typeDefinition(field)', 'textDocument/typeDefinition', { textDocument: { uri: controller }, position: pos(3, 27) }],
  ['definition(xml -> java)', 'textDocument/definition', { textDocument: { uri: mapperXml }, position: pos(4, 20) }],
  ['definition(java -> xml)', 'textDocument/definition', { textDocument: { uri: mapperJava }, position: pos(3, 12) }],
  ['documentHighlight', 'textDocument/documentHighlight', { textDocument: { uri: controller }, position: pos(7, 30) }],
  ['hover', 'textDocument/hover', { textDocument: { uri: controller }, position: pos(7, 30) }],
  ['documentSymbol', 'textDocument/documentSymbol', { textDocument: { uri: service } }],
  ['workspaceSymbol', 'workspace/symbol', { query: 'find' }],
];
for (const [label, method, params] of cases) {
  console.log(`${label}:`, result(await send(method, params))?.slice(0, 420));
}

for (const [label, roots] of [['all', []], ['src', ['testdata/demo1/src']]]) {
  console.log(`referencesInPaths(${label}):`, result(await send('workspace/executeCommand', {
    command: 'ld-java-analyze.referencesInPaths',
    arguments: [{ uri: controller, line: 7, character: 30, roots, includeDeclaration: true }],
  })));
}

// ---------------------------------------------------------- scan rules checks
let failures = 0;
function check(label, condition) {
  const ok = Boolean(condition);
  console.log(`${ok ? 'PASS' : 'FAIL'}: ${label}`);
  if (!ok) failures += 1;
}
const command = async (name, argument) =>
  (await send('workspace/executeCommand', { command: name, arguments: [argument] }))?.result;
const stats = () => command('ld-java-analyze.stats', {});
const rules = ['generated/', 'file:*.generated.java'];

const initial = await stats();
console.log('scan rules at startup:', JSON.stringify(initial?.effectiveRules));
check('启动时规则过滤掉 generated/ 与 *.generated.java', initial?.files === 6);
check(
  '被排除目录的符号不出现在 workspace/symbol',
  (await send('workspace/symbol', { query: 'Noise' }))?.result?.length === 0,
);
const listed = await command('ld-java-analyze.getExcludeRules', {});
check('getExcludeRules 返回配置规则', listed?.excludeRules?.length === 2);

const cleared = await command('ld-java-analyze.setExcludeRules', []);
check('清空规则后多扫描到 2 个文件', cleared?.files === initial.files + 2);
check(
  '清空后 Noise 类可被搜索到',
  (await send('workspace/symbol', { query: 'Noise' }))?.result?.some(
    (symbol) => symbol.name === 'Noise',
  ) === true,
);

const restored = await command('ld-java-analyze.setExcludeRules', rules);
check('恢复规则后回到基线文件数', restored?.files === initial.files);

await send('workspace/didChangeConfiguration', {
  settings: { 'ld-java-analyze': { excludeRules: [] } },
}, false);
await new Promise((resolve) => setTimeout(resolve, 250));
const afterConfig = await stats();
check('didChangeConfiguration 让配置变更即时生效', afterConfig?.files === initial.files + 2);

await send('shutdown', null);
await send('exit', {}, false);
server.stdin.end();
if (stderr.trim()) console.log('stderr:', stderr.trim().slice(0, 500));
setTimeout(() => process.exit(failures === 0 ? 0 : 1), 300);
