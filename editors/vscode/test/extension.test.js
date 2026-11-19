// Runs the extension against a small fake of the `vscode` API and the real `tau` compiler
// (TAU_BIN, or target/debug/tau, or target/release/tau of this repository).
'use strict';
const fs = require('fs');
const path = require('path');
const assert = require('assert');
const Module = require('module');

const repo = path.join(__dirname, '..', '..', '..');
const tauBin = [process.env.TAU_BIN, path.join(repo, 'target', 'debug', 'tau'), path.join(repo, 'target', 'release', 'tau')].find((p) => p && fs.existsSync(p));
if (!tauBin) {
  console.log('skipped: no tau binary (run `cargo build` first or set TAU_BIN)');
  process.exit(0);
}

// ------------------------------------------------------------------ a fake vscode
class Position {
  constructor(l, c) { this.line = l; this.character = c; }
  isBefore(o) { return this.line < o.line || (this.line === o.line && this.character < o.character); }
}
class Range {
  constructor(a, b, c, d) {
    if (typeof a === 'number') { this.start = new Position(a, b); this.end = new Position(c, d); }
    else { this.start = a; this.end = b; }
  }
  contains(x) {
    if (x instanceof Range) return !x.start.isBefore(this.start) && !this.end.isBefore(x.end);
    return !x.isBefore(this.start) && !this.end.isBefore(x);
  }
  with() { return this; }
}
const config = { path: tauBin, checkOnType: true, checkDelayMs: 0, codeLens: true, hover: true, runInTerminal: true, threads: null };
const registered = {};
const diagStore = new Map();
const sent = [];
const vscode = {
  Position, Range,
  Uri: class Uri { static file(p) { const u = new Uri(); u.scheme = 'file'; u.fsPath = p; return u; } static parse(s) { return Uri.file(s.replace('file://', '')); } toString() { return 'file://' + this.fsPath; } },
  Diagnostic: class { constructor(range, message, severity) { Object.assign(this, { range, message, severity }); } },
  DiagnosticRelatedInformation: class { constructor(location, message) { Object.assign(this, { location, message }); } },
  DiagnosticSeverity: { Error: 0, Warning: 1 },
  Location: class { constructor(uri, range) { Object.assign(this, { uri, range }); } },
  Hover: class { constructor(contents, range) { Object.assign(this, { contents, range }); } },
  MarkdownString: class { constructor() { this.value = ''; } appendCodeblock(c, l) { this.value += '```' + l + '\n' + c + '\n```'; return this; } },
  CodeLens: class { constructor(range, command) { Object.assign(this, { range, command }); } },
  DocumentSymbol: class { constructor(name, detail, kind, range, sel) { Object.assign(this, { name, detail, kind, range, selectionRange: sel, children: [] }); } },
  SymbolKind: { Function: 11, Struct: 22, Enum: 9, Interface: 10, Constant: 13, Class: 4, Namespace: 2 },
  StatusBarAlignment: { Right: 2 },
  ViewColumn: { Beside: -2 },
  languages: {
    createDiagnosticCollection: () => ({ set: (u, l) => diagStore.set(u.toString(), l), get: (u) => diagStore.get(u.toString()), delete: (u) => diagStore.delete(u.toString()), dispose() {} }),
    registerHoverProvider: (_, p) => ((registered.hover = p), { dispose() {} }),
    registerDocumentSymbolProvider: (_, p) => ((registered.symbols = p), { dispose() {} }),
    registerCodeLensProvider: (_, p) => ((registered.lens = p), { dispose() {} }),
  },
  window: {
    createOutputChannel: () => ({ appendLine() {}, append() {}, show() {}, dispose() {} }),
    createStatusBarItem: () => ({ show() {}, hide() {}, dispose() {} }),
    createTerminal: () => ({ show() {}, sendText: (t) => sent.push(t), exitStatus: undefined }),
    showInformationMessage() {}, showWarningMessage: async () => undefined, showErrorMessage() {},
    activeTextEditor: undefined, onDidChangeActiveTextEditor: () => ({ dispose() {} }),
  },
  workspace: {
    textDocuments: [],
    getConfiguration: () => ({ get: (k) => config[k] }),
    onDidOpenTextDocument: () => ({ dispose() {} }), onDidSaveTextDocument: () => ({ dispose() {} }),
    onDidChangeTextDocument: () => ({ dispose() {} }), onDidCloseTextDocument: () => ({ dispose() {} }),
    onDidChangeConfiguration: () => ({ dispose() {} }),
    openTextDocument: async (u) => makeDoc(u.fsPath, fs.readFileSync(u.fsPath, 'utf8')),
  },
  commands: { registerCommand: (n, f) => ((registered[n] = f), { dispose() {} }), executeCommand() {} },
};
const realLoad = Module._load;
Module._load = function (req, ...rest) { return req === 'vscode' ? vscode : realLoad.call(this, req, ...rest); };

function makeDoc(file, text) {
  const lines = text.split('\n');
  return {
    languageId: 'tau', uri: vscode.Uri.file(file), version: 1, isDirty: false, lineCount: lines.length,
    getText: () => text, save: async () => true,
    lineAt: (i) => ({ text: lines[i], range: new Range(i, 0, i, lines[i].length) }),
  };
}

const ext = require('../extension.js');
const tests = [];
const test = (n, f) => tests.push([n, f]);
const tmp = fs.mkdtempSync(path.join(require('os').tmpdir(), 'tau-ext-'));
ext.activate({ subscriptions: [] });

const GOOD = `fn add(a: i32, b: i32) -> i32 { a + b }

struct P { x: f64 }

impl P {
    fn norm(self) -> f64 { self.x }
}

#[test]
fn adds() {
    assert_eq(add(1, 2), 3)
}

fn main() {
    let x = add(1, 2)
    print(x)
}
`;

const settle = () => new Promise((r) => setTimeout(r, 1500));

test('a correct file has no diagnostics and gives hovers', async () => {
  const doc = makeDoc(path.join(tmp, 'good.tau'), GOOD);
  vscode.workspace.textDocuments.push(doc);
  // tau.check given a Uri reads the file from disk
  fs.writeFileSync(doc.uri.fsPath, GOOD);
  await registered['tau.check'](doc.uri);
  await settle();
  assert.deepStrictEqual(diagStore.get(doc.uri.toString()), []);
  const h = registered.hover.provideHover(doc, new Position(14, 8));
  assert(h, 'a hover on `x` in `let x`');
  assert.match(h.contents.value, /x: i32/);
  const sig = registered.hover.provideHover(doc, new Position(0, 4));
  assert.match(sig.contents.value, /fn add\(a: i32, b: i32\) -> i32/);
  assert.strictEqual(registered.hover.provideHover(doc, new Position(1, 0)), undefined);
});

test('a type error becomes a diagnostic at the right range', async () => {
  const bad = GOOD.replace('add(1, 2)\n    print', 'add(1, "a")\n    print');
  const doc = makeDoc(path.join(tmp, 'bad.tau'), bad);
  fs.writeFileSync(doc.uri.fsPath, bad);
  await registered['tau.check'](doc.uri);
  await settle();
  const list = diagStore.get(doc.uri.toString());
  assert.strictEqual(list.length, 1, JSON.stringify(list));
  assert.match(list[0].message, /mismatched types/);
  assert.strictEqual(list[0].range.start.line, 14);
  assert.strictEqual(list[0].source, 'tau');
  assert.strictEqual(list[0].range.start.character, 19);
});

test('a file that uses another file of its package is checked with the package', async () => {
  const proj = path.join(tmp, 'proj');
  fs.mkdirSync(path.join(proj, 'src'), { recursive: true });
  fs.writeFileSync(path.join(proj, 'tau.toml'), '[package]\nname = "p"\n');
  const main = 'use util\n\nfn main() {\n    print(util.double(21))\n}\n';
  fs.writeFileSync(path.join(proj, 'src', 'main.tau'), main);
  fs.writeFileSync(path.join(proj, 'src', 'util.tau'), 'pub fn double(x: i64) -> i64 {\n    "oops"\n}\n');
  const mainUri = vscode.Uri.file(path.join(proj, 'src', 'main.tau'));
  const utilUri = vscode.Uri.file(path.join(proj, 'src', 'util.tau'));
  await registered['tau.check'](mainUri);
  await settle();
  // no false "unresolved import" on main.tau, and the real error is shown on util.tau
  assert.deepStrictEqual(diagStore.get(mainUri.toString()), []);
  const onUtil = diagStore.get(utilUri.toString());
  assert(onUtil && onUtil.length === 1, JSON.stringify(onUtil));
  assert.match(onUtil[0].message, /mismatched types/);
  assert.strictEqual(onUtil[0].range.start.line, 1);
  // after the fix the error on util.tau goes away
  fs.writeFileSync(path.join(proj, 'src', 'util.tau'), 'pub fn double(x: i64) -> i64 { x * 2 }\n');
  await registered['tau.check'](mainUri);
  await settle();
  assert.deepStrictEqual(diagStore.get(utilUri.toString()) || [], []);
});

test('code lenses sit on main and on tests', async () => {
  const lenses = registered.lens.provideCodeLenses(makeDoc('x.tau', GOOD));
  const titles = lenses.map((l) => `${l.range.start.line}:${l.command.title}`);
  assert(titles.includes('13:$(play) Run'), titles.join(', '));
  assert(titles.includes('9:$(beaker) Run test'), titles.join(', '));
  assert.strictEqual(lenses.filter((l) => l.command.command === 'tau.test').length, 1);
  config.codeLens = false;
  assert.deepStrictEqual(registered.lens.provideCodeLenses(makeDoc('x.tau', GOOD)), []);
  config.codeLens = true;
});

test('the outline lists items and nests methods in their impl', async () => {
  const syms = registered.symbols.provideDocumentSymbols(makeDoc('x.tau', GOOD));
  const names = syms.map((s) => s.name);
  assert.deepStrictEqual(names, ['add', 'P', 'impl P', 'adds', 'main']);
  const impl = syms.find((s) => s.name === 'impl P');
  assert.deepStrictEqual(impl.children.map((c) => c.name), ['norm']);
});

test('run and test commands go to the terminal with the file quoted', async () => {
  const file = path.join(tmp, 'good.tau');
  fs.writeFileSync(file, GOOD);
  await registered['tau.run'](vscode.Uri.file(file));
  await registered['tau.test'](vscode.Uri.file(file), 'adds');
  assert.match(sent[0], /run /);
  assert(sent[0].includes('good.tau'));
  assert.match(sent[1], /test .*good\.tau.* .adds./);
});

test('a missing compiler is reported without throwing', async () => {
  const old = config.path;
  config.path = path.join(tmp, 'no-such-tau');
  const doc = makeDoc(path.join(tmp, 'good.tau'), GOOD);
  fs.writeFileSync(doc.uri.fsPath, GOOD);
  await registered['tau.check'](doc.uri);
  await settle();
  config.path = old;
});

(async () => {
  let failed = 0;
  for (const [name, fn] of tests) {
    try { await fn(); console.log(`ok   ${name}`); } catch (e) { failed++; console.log(`FAIL ${name}\n     ${String(e.stack || e).split('\n').slice(0, 6).join('\n     ')}`); }
  }
  console.log(`${tests.length - failed}/${tests.length} extension tests passed`);
  ext.deactivate();
  fs.rmSync(tmp, { recursive: true, force: true });
  process.exit(failed ? 1 : 0);
})();
