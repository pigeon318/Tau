// The Tau extension for VS Code. Plain JavaScript, no dependencies: it asks the `tau` compiler
// to check the file (`tau check - --json`, the program on stdin) and shows what it says.
'use strict';

const vscode = require('vscode');
const cp = require('child_process');
const path = require('path');

/** @type {vscode.DiagnosticCollection} */
let diagnostics;
/** Last compiler answer per document URI: { version, hovers } */
const lastResult = new Map();
/** The other files each document's last check reported errors on (URIs), to clear them later. */
const otherFiles = new Map();
/** Pending (debounced) checks per document URI. */
const timers = new Map();
/** Running compiler processes per document URI, so a newer check cancels an older one. */
const running = new Map();
let statusItem;
let output;

function config() {
  return vscode.workspace.getConfiguration('tau');
}

function tauPath() {
  return config().get('path') || 'tau';
}

function log(msg) {
  output.appendLine(`[${new Date().toISOString().slice(11, 19)}] ${msg}`);
}

// ------------------------------------------------------------------------- checking

function toRange(r) {
  return new vscode.Range(r.start.line, r.start.character, r.end.line, r.end.character);
}

let warnedMissing = false;

async function checkDocument(doc) {
  if (doc.languageId !== 'tau') return;
  const key = doc.uri.toString();
  const prev = running.get(key);
  if (prev) prev.kill();
  const cwd = doc.uri.scheme === 'file' ? path.dirname(doc.uri.fsPath) : undefined;
  const filePath = doc.uri.scheme === 'file' ? doc.uri.fsPath : undefined;
  const version = doc.version;
  let result;
  try {
    result = await runCheckFor(key, doc.getText(), cwd, filePath);
  } catch (e) {
    if (e && e.message === 'cancelled') return;
    if (e && e.code === 'ENOENT') {
      setStatus('$(warning) Tau: compiler not found', `Could not run "${tauPath()}". Set "tau.path" in settings.`);
      if (!warnedMissing) {
        warnedMissing = true;
        const pick = await vscode.window.showWarningMessage(
          `Tau: could not run "${tauPath()}". Install the compiler or set "tau.path".`,
          'Open Settings'
        );
        if (pick) vscode.commands.executeCommand('workbench.action.openSettings', 'tau.path');
      }
    } else {
      setStatus('$(warning) Tau', String(e && e.message ? e.message : e));
      log(String(e && e.stack ? e.stack : e));
    }
    return;
  }
  // the text may have changed while the compiler ran; a newer check is on its way then
  if (doc.version !== version) return;
  // The compiler checks the whole package, so errors can be in other files. Errors in this file are
  // shown on it; errors in other files are shown on those files unless they are open (an open file is
  // checked by itself, with its own unsaved text).
  const here = samePath(filePath);
  const byFile = new Map(); // fsPath ('' for this document) -> diagnostics
  for (const d of result.diagnostics || []) {
    const diag = new vscode.Diagnostic(
      toRange(d.range),
      d.hint ? `${d.message}\nhelp: ${d.hint}` : d.message,
      d.severity === 'warning' ? vscode.DiagnosticSeverity.Warning : vscode.DiagnosticSeverity.Error
    );
    diag.source = 'tau';
    if (d.related && d.related.length) {
      diag.relatedInformation = d.related.map((n) => {
        const target = n.file && !here(n.file) ? vscode.Uri.file(n.file) : doc.uri;
        return new vscode.DiagnosticRelatedInformation(new vscode.Location(target, toRange(n.range)), n.message);
      });
    }
    const where = !d.file || here(d.file) ? '' : d.file;
    if (!byFile.has(where)) byFile.set(where, []);
    byFile.get(where).push(diag);
  }
  diagnostics.set(doc.uri, byFile.get('') || []);
  const open = new Set(vscode.workspace.textDocuments.map((t) => t.uri.toString()));
  const touched = new Set();
  for (const [file, list] of byFile) {
    if (file === '') continue;
    const uri = vscode.Uri.file(file);
    if (open.has(uri.toString())) continue;
    diagnostics.set(uri, list);
    touched.add(uri.toString());
  }
  // files that this document's last check reported on and that are fine now
  for (const old of otherFiles.get(key) || []) {
    if (!touched.has(old) && !open.has(old)) diagnostics.delete(vscode.Uri.parse(old));
  }
  otherFiles.set(key, touched);
  const elsewhere = [...byFile].filter(([f]) => f !== '').reduce((n, [, l]) => n + l.length, 0);
  lastResult.set(key, { version, hovers: result.hovers || [], elsewhere });
  updateStatus(vscode.window.activeTextEditor);
}

/** A function telling whether a path is the same file as `mine` (case-insensitive on Windows). */
function samePath(mine) {
  const norm = (p) => (process.platform === 'win32' ? path.resolve(p).toLowerCase() : path.resolve(p));
  const m = mine ? norm(mine) : undefined;
  return (other) => m !== undefined && other !== undefined && norm(other) === m;
}

function runCheckFor(key, text, cwd, filePath) {
  return new Promise((resolve, reject) => {
    let child;
    try {
      // `--file` says which file the text is, so that it is checked as part of its package
      const args = ['check', '-', '--json'].concat(filePath ? ['--file', filePath] : []);
      child = cp.spawn(tauPath(), args, { cwd, windowsHide: true });
    } catch (e) {
      reject(e);
      return;
    }
    running.set(key, child);
    let out = '';
    let err = '';
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (d) => (out += d));
    child.stderr.on('data', (d) => (err += d));
    child.on('error', (e) => {
      if (running.get(key) === child) running.delete(key);
      reject(e);
    });
    child.on('close', (code, signal) => {
      if (running.get(key) === child) running.delete(key);
      if (signal) return reject(new Error('cancelled'));
      try {
        resolve(JSON.parse(out));
      } catch (e) {
        reject(new Error(`tau exited with ${code} and printed no usable output: ${(err || out).trim().slice(0, 300)}`));
      }
    });
    child.stdin.on('error', () => {});
    child.stdin.end(text);
  });
}

function scheduleCheck(doc) {
  if (doc.languageId !== 'tau') return;
  const key = doc.uri.toString();
  clearTimeout(timers.get(key));
  const delay = Math.max(0, Number(config().get('checkDelayMs')) || 0);
  timers.set(
    key,
    setTimeout(() => {
      timers.delete(key);
      checkDocument(doc);
    }, delay)
  );
}

// ------------------------------------------------------------------------- status bar

function setStatus(text, tooltip) {
  statusItem.text = text;
  statusItem.tooltip = tooltip || '';
  statusItem.show();
}

function updateStatus(editor) {
  if (!editor || editor.document.languageId !== 'tau') {
    statusItem.hide();
    return;
  }
  const list = diagnostics.get(editor.document.uri) || [];
  const res = lastResult.get(editor.document.uri.toString());
  const elsewhere = (res && res.elsewhere) || 0;
  const other = elsewhere ? ` (+${elsewhere} in other files)` : '';
  if (!res) {
    setStatus('$(sync~spin) Tau', 'Checking...');
  } else if (list.length === 0 && elsewhere === 0) {
    setStatus('$(check) Tau', 'No errors. Click to check again.');
  } else if (list.length === 0) {
    setStatus(`$(warning) Tau: ${elsewhere} in other files`, `No errors in this file, but ${elsewhere} in other files of the package. Click to check again.`);
  } else {
    setStatus(`$(error) Tau: ${list.length}${other}`, `${list.length} error(s) in this file${other}. Click to check again.`);
  }
}

// ------------------------------------------------------------------------- hover

const hoverProvider = {
  provideHover(doc, pos) {
    if (!config().get('hover')) return undefined;
    const res = lastResult.get(doc.uri.toString());
    if (!res) return undefined;
    // use the smallest range containing the position
    let best;
    for (const h of res.hovers) {
      const r = toRange(h.range);
      if (r.contains(pos) && (!best || best.range.contains(r))) best = { range: r, text: h.text };
    }
    if (!best) return undefined;
    // the answer is stale when the document has been edited since: do not point at the wrong text
    if (res.version !== doc.version) return undefined;
    return new vscode.Hover(new vscode.MarkdownString().appendCodeblock(best.text, 'tau'), best.range);
  },
};

// ------------------------------------------------------------------------- outline and code lenses

const DECL = /^(\s*)(?:pub\s+)?(?:unsafe\s+)?(fn|struct|enum|trait|const|type|cluster)\s+([A-Za-z_][A-Za-z0-9_]*)/;
const IMPL = /^\s*impl\b(?:\s*<[^>]*>)?\s+(.+?)\s*\{?\s*$/;
const KIND = {
  fn: vscode.SymbolKind.Function,
  struct: vscode.SymbolKind.Struct,
  enum: vscode.SymbolKind.Enum,
  trait: vscode.SymbolKind.Interface,
  const: vscode.SymbolKind.Constant,
  type: vscode.SymbolKind.Class,
  cluster: vscode.SymbolKind.Namespace,
};

/** Blank out comments and strings so the line scanners are not fooled by them. */
function code(line) {
  return line.replace(/\/\/.*$/, '').replace(/"(?:[^"\\]|\\.)*"/g, '""');
}

const symbolProvider = {
  provideDocumentSymbols(doc) {
    const symbols = [];
    let impl; // the open impl block, if any
    let depth = 0;
    let implDepth = -1;
    for (let i = 0; i < doc.lineCount; i++) {
      const line = code(doc.lineAt(i).text);
      let m = IMPL.exec(line);
      if (m && depth === 0) {
        impl = new vscode.DocumentSymbol(`impl ${m[1].replace(/\s*\{$/, '')}`, '', vscode.SymbolKind.Class, doc.lineAt(i).range, doc.lineAt(i).range);
        symbols.push(impl);
        implDepth = depth;
      } else if ((m = DECL.exec(line))) {
        const name = m[3];
        const range = doc.lineAt(i).range;
        const sym = new vscode.DocumentSymbol(name, m[2], KIND[m[2]], range, doc.lineAt(i).range.with(undefined, undefined));
        if (impl && depth > implDepth) impl.children.push(sym);
        else symbols.push(sym);
      }
      for (const ch of line) {
        if (ch === '{') depth++;
        else if (ch === '}') {
          depth--;
          if (impl && depth <= implDepth) {
            // extend the impl's range to its closing brace
            impl.range = new vscode.Range(impl.range.start, doc.lineAt(i).range.end);
            impl = undefined;
          }
        }
      }
    }
    return symbols;
  },
};

const lensProvider = {
  provideCodeLenses(doc) {
    if (!config().get('codeLens')) return [];
    const lenses = [];
    let testNext = false;
    for (let i = 0; i < doc.lineCount; i++) {
      const raw = doc.lineAt(i).text;
      const line = code(raw);
      if (/^\s*#\[\s*test\s*\]/.test(line)) {
        testNext = true;
        // `#[test] fn name()` on one line
        const same = /\bfn\s+([A-Za-z_][A-Za-z0-9_]*)/.exec(line);
        if (same) {
          pushTestLens(lenses, doc, i, same[1]);
          testNext = false;
        }
        continue;
      }
      const f = /^\s*(?:pub\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)/.exec(line);
      if (f) {
        const range = new vscode.Range(i, 0, i, 0);
        if (testNext) {
          pushTestLens(lenses, doc, i, f[1]);
        } else if (f[1] === 'main' && /^fn\b|^pub\b/.test(raw)) {
          lenses.push(new vscode.CodeLens(range, { title: '$(play) Run', command: 'tau.run', arguments: [doc.uri] }));
          lenses.push(new vscode.CodeLens(range, { title: 'Run (interpreter)', command: 'tau.runInterpreter', arguments: [doc.uri] }));
        }
        testNext = false;
      } else if (line.trim() !== '' && !/^\s*#\[/.test(line)) {
        testNext = false;
      }
    }
    return lenses;
  },
};

function pushTestLens(lenses, doc, line, name) {
  const range = new vscode.Range(line, 0, line, 0);
  lenses.push(new vscode.CodeLens(range, { title: '$(beaker) Run test', command: 'tau.test', arguments: [doc.uri, name] }));
}

// ------------------------------------------------------------------------- commands

let terminal;

function getTerminal() {
  if (!terminal || terminal.exitStatus !== undefined) {
    const env = {};
    const threads = config().get('threads');
    if (threads) env.TAU_THREADS = String(threads);
    terminal = vscode.window.createTerminal({ name: 'Tau', env });
  }
  return terminal;
}

/** Quote one argument for the shell the terminal is using. */
function quote(arg) {
  if (process.platform === 'win32') {
    // PowerShell and cmd both accept double quotes around a path
    return `"${arg.replace(/"/g, '\\"')}"`;
  }
  return `'${arg.replace(/'/g, `'\\''`)}'`;
}

function exe() {
  const p = tauPath();
  if (process.platform === 'win32') {
    // PowerShell needs `&` to run a quoted path
    return /\s/.test(p) ? `& ${quote(p)}` : p;
  }
  return /\s/.test(p) ? quote(p) : p;
}

async function documentFor(uri) {
  let doc;
  if (uri instanceof vscode.Uri) {
    doc = await vscode.workspace.openTextDocument(uri);
  } else if (vscode.window.activeTextEditor) {
    doc = vscode.window.activeTextEditor.document;
  }
  if (!doc || doc.languageId !== 'tau') {
    vscode.window.showInformationMessage('Open a .tau file first.');
    return undefined;
  }
  if (doc.uri.scheme !== 'file') {
    vscode.window.showInformationMessage('Save the file to disk first.');
    return undefined;
  }
  if (doc.isDirty) await doc.save();
  return doc;
}

/** Run a tau command on a file, in the terminal or in the output panel. */
async function runTau(uri, args, opts = {}) {
  const doc = await documentFor(uri);
  if (!doc) return;
  const file = doc.uri.fsPath;
  if (config().get('runInTerminal') || opts.terminal) {
    const t = getTerminal();
    t.show(true);
    t.sendText(`${exe()} ${args[0]} ${quote(file)}${args.slice(1).map((a) => ' ' + quote(a)).join('')}`);
    return;
  }
  output.show(true);
  output.appendLine(`> tau ${args[0]} ${file}`);
  const child = cp.spawn(tauPath(), [args[0], file, ...args.slice(1)], { cwd: path.dirname(file), windowsHide: true });
  child.stdout.on('data', (d) => output.append(String(d)));
  child.stderr.on('data', (d) => output.append(String(d)));
  child.on('error', (e) => output.appendLine(`could not run tau: ${e.message}`));
  child.on('close', (code) => output.appendLine(`[exit ${code}]`));
}

async function showGeneratedC(uri) {
  const doc = await documentFor(uri);
  if (!doc) return;
  cp.execFile(tauPath(), ['build', doc.uri.fsPath, '--emit-c'], { cwd: path.dirname(doc.uri.fsPath), maxBuffer: 64 * 1024 * 1024, windowsHide: true }, async (err, stdout, stderr) => {
    if (err && !stdout) {
      vscode.window.showErrorMessage(`tau build failed: ${(stderr || err.message).split('\n')[0]}`);
      return;
    }
    const c = await vscode.workspace.openTextDocument({ language: 'c', content: stdout });
    vscode.window.showTextDocument(c, { viewColumn: vscode.ViewColumn.Beside, preview: true });
  });
}

function showVersion() {
  cp.execFile(tauPath(), ['--version'], { windowsHide: true }, (err, stdout) => {
    if (err) {
      vscode.window.showErrorMessage(`Could not run "${tauPath()}": ${err.message}`);
    } else {
      vscode.window.showInformationMessage(stdout.trim().replace(/\s+/g, ' '));
    }
  });
}

// ------------------------------------------------------------------------- activation

function activate(context) {
  output = vscode.window.createOutputChannel('Tau');
  diagnostics = vscode.languages.createDiagnosticCollection('tau');
  statusItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 50);
  statusItem.command = 'tau.check';
  context.subscriptions.push(output, diagnostics, statusItem);

  const tau = { language: 'tau' };
  context.subscriptions.push(
    vscode.languages.registerHoverProvider(tau, hoverProvider),
    vscode.languages.registerDocumentSymbolProvider(tau, symbolProvider),
    vscode.languages.registerCodeLensProvider(tau, lensProvider),

    vscode.workspace.onDidOpenTextDocument((d) => scheduleCheck(d)),
    vscode.workspace.onDidSaveTextDocument((d) => checkDocument(d)),
    vscode.workspace.onDidChangeTextDocument((e) => {
      if (config().get('checkOnType')) scheduleCheck(e.document);
    }),
    vscode.workspace.onDidCloseTextDocument((d) => {
      const key = d.uri.toString();
      diagnostics.delete(d.uri);
      lastResult.delete(key);
      clearTimeout(timers.get(key));
      timers.delete(key);
      const p = running.get(key);
      if (p) p.kill();
    }),
    vscode.window.onDidChangeActiveTextEditor((e) => updateStatus(e)),
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration('tau.path')) {
        warnedMissing = false;
        vscode.workspace.textDocuments.forEach(scheduleCheck);
      }
    }),

    vscode.commands.registerCommand('tau.run', (uri) => runTau(uri, ['run'])),
    vscode.commands.registerCommand('tau.runInterpreter', (uri) => runTau(uri, ['run', '--interp'])),
    vscode.commands.registerCommand('tau.test', (uri, name) => runTau(uri, name ? ['test', name] : ['test'])),
    vscode.commands.registerCommand('tau.build', (uri) => runTau(uri, ['build'], { terminal: true })),
    vscode.commands.registerCommand('tau.check', async (uri) => {
      const doc = uri instanceof vscode.Uri ? await vscode.workspace.openTextDocument(uri) : vscode.window.activeTextEditor && vscode.window.activeTextEditor.document;
      if (doc) checkDocument(doc);
    }),
    vscode.commands.registerCommand('tau.emitC', (uri) => showGeneratedC(uri)),
    vscode.commands.registerCommand('tau.clean', () => {
      cp.execFile(tauPath(), ['clean'], { windowsHide: true }, (err, stdout) => {
        if (err) vscode.window.showErrorMessage(`Could not run "${tauPath()}": ${err.message}`);
        else vscode.window.showInformationMessage(`Tau: ${stdout.trim()}`);
      });
    }),
    vscode.commands.registerCommand('tau.version', showVersion)
  );

  vscode.workspace.textDocuments.forEach(scheduleCheck);
  updateStatus(vscode.window.activeTextEditor);
}

function deactivate() {
  for (const p of running.values()) p.kill();
  for (const t of timers.values()) clearTimeout(t);
}

module.exports = { activate, deactivate };
