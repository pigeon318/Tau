// Tokenizes small Tau snippets with the real TextMate engine VS Code uses and checks the scopes.
'use strict';
const fs = require('fs');
const path = require('path');
const assert = require('assert');
const vsctm = require('vscode-textmate');
const oniguruma = require('vscode-oniguruma');

const wasm = fs.readFileSync(require.resolve('vscode-oniguruma/release/onig.wasm')).buffer;
const onigLib = oniguruma.loadWASM(wasm).then(() => ({
  createOnigScanner: (p) => new oniguruma.OnigScanner(p),
  createOnigString: (s) => new oniguruma.OnigString(s),
}));
const registry = new vsctm.Registry({
  onigLib,
  loadGrammar: async (scope) =>
    scope === 'source.tau' ? vsctm.parseRawGrammar(fs.readFileSync(path.join(__dirname, '..', 'syntaxes', 'tau.tmLanguage.json'), 'utf8'), 'tau.tmLanguage.json') : null,
});

/** [[text, scopes...]] for every token of every line. */
async function tokenize(src) {
  const g = await registry.loadGrammar('source.tau');
  let stack = vsctm.INITIAL;
  const out = [];
  for (const line of src.split('\n')) {
    const r = g.tokenizeLine(line, stack);
    for (const t of r.tokens) out.push([line.slice(t.startIndex, t.endIndex), t.scopes.slice(1)]);
    stack = r.ruleStack;
  }
  return out;
}

/** The innermost scope of the first token with this text. */
function scopeOf(tokens, text, nth = 0) {
  const hits = tokens.filter(([t]) => t === text);
  assert(hits.length > nth, `no token ${JSON.stringify(text)} in ${JSON.stringify(tokens.map((t) => t[0]))}`);
  return hits[nth][1].join(' ');
}

const cases = [];
function test(name, fn) {
  cases.push([name, fn]);
}

test('keywords, declarations and types', async () => {
  const t = await tokenize('pub fn area(s: Shape, k: f64) -> f64 {\n  let mut x = 1\n}');
  assert.match(scopeOf(t, 'pub'), /storage\.modifier/);
  assert.match(scopeOf(t, 'fn'), /storage\.type\.function/);
  assert.match(scopeOf(t, 'area'), /entity\.name\.function/);
  assert.match(scopeOf(t, 'Shape'), /entity\.name\.type/);
  assert.match(scopeOf(t, 'f64'), /support\.type\.primitive/);
  assert.match(scopeOf(t, 'let'), /storage\.type\.let/);
  assert.match(scopeOf(t, 'mut'), /storage\.modifier\.mut/);
  assert.match(scopeOf(t, '->'), /keyword\.operator\.arrow/);
});

test('numbers', async () => {
  const t = await tokenize('1_000 0xFF 0b1010 0o17 3.14 1e-9 2.5e3f32 7u8 4i 1_000usize');
  for (const n of ['1_000', '0xFF', '0b1010', '0o17', '3.14', '1e-9', '2.5e3f32', '7u8', '4i', '1_000usize']) {
    assert.match(scopeOf(t, n), /constant\.numeric/, n);
  }
  assert.match(scopeOf(t, '0xFF'), /hex/);
  assert.match(scopeOf(t, '3.14'), /float/);
  assert.match(scopeOf(t, '1_000'), /integer/);
});

test('a range is not a float', async () => {
  const t = await tokenize('for i in 0..10 { }');
  assert.match(scopeOf(t, '0'), /integer/);
  assert.match(scopeOf(t, '..'), /range/);
  assert.match(scopeOf(t, '10'), /integer/);
});

test('comments, including nested block comments', async () => {
  const t = await tokenize('// line fn\n/* a /* b */ still comment */ let');
  assert.match(scopeOf(t, '// line fn'), /comment\.line/);
  assert.match(scopeOf(t, 'let'), /storage\.type\.let/);
  const inner = t.filter(([x]) => x.includes('still comment'));
  assert(inner.length && /comment\.block/.test(inner[0][1].join(' ')), JSON.stringify(t));
});

test('strings with escapes and interpolation', async () => {
  const t = await tokenize('print("a\\n{x+1} {{b}} {s.len}")');
  assert.match(scopeOf(t, '\\n'), /constant\.character\.escape/);
  assert.match(scopeOf(t, 'x'), /meta\.interpolation/);
  assert.match(scopeOf(t, '1'), /constant\.numeric/);
  assert.match(scopeOf(t, '{{'), /escape\.brace/);
  assert.match(scopeOf(t, 'len'), /meta\.interpolation/);
  assert.match(scopeOf(t, 'print'), /support\.function/);
});

test('a string inside an interpolation', async () => {
  const t = await tokenize('"x {f("}")} y" let');
  assert.match(scopeOf(t, 'let'), /storage\.type\.let/);
});

test('char literals', async () => {
  const t = await tokenize("let c = 'a'\nlet d = '\\n'");
  assert.match(scopeOf(t, "'a'"), /string\.quoted\.single/);
  assert.match(scopeOf(t, "'\\n'"), /string\.quoted\.single/);
});

test('attributes', async () => {
  const t = await tokenize('#[test]\nfn adds() {}');
  assert.match(scopeOf(t, 'test'), /entity\.other\.attribute-name/);
  assert.match(scopeOf(t, 'adds'), /entity\.name\.function/);
});

test('operators', async () => {
  const t = await tokenize('a @ b ^ 2 ^^ c <= d && e || !f x?');
  assert.match(scopeOf(t, '@'), /matmul/);
  assert.match(scopeOf(t, '^'), /power/);
  assert.match(scopeOf(t, '^^'), /arithmetic/);
  assert.match(scopeOf(t, '<='), /comparison/);
  assert.match(scopeOf(t, '&&'), /logical/);
  assert.match(scopeOf(t, '?'), /try/);
});

test('parallel constructs and ownership', async () => {
  const t = await tokenize('par for i in 0..n { }\nlet s = par sum(i in 0..n) a[i]\nlet h = spawn { 1 }\nfn f(own v: Vec<f64>, w: mut [f64]) {}');
  assert.match(scopeOf(t, 'par'), /parallel/);
  assert.match(scopeOf(t, 'spawn'), /parallel/);
  assert.match(scopeOf(t, 'own'), /storage\.modifier/);
  assert.match(scopeOf(t, 'Vec'), /support\.type\.builtin/);
});

test('types, variants and calls', async () => {
  const t = await tokenize('let r: Result<i32, String> = Ok(Vec3(1.0, 2.0, 3.0))\nv.push(1)\nx.0');
  assert.match(scopeOf(t, 'Result'), /support\.type\.builtin/);
  assert.match(scopeOf(t, 'Ok'), /enum-variant/);
  assert.match(scopeOf(t, 'Vec3'), /entity\.name\.type/);
  assert.match(scopeOf(t, 'push'), /method/);
});

test('every example and test program tokenizes without leaving a string or comment open', async () => {
  const root = path.join(__dirname, '..', '..', '..');
  const files = [];
  for (const dir of ['examples', 'tests/fuzz_corpus', 'examples/bench']) {
    const d = path.join(root, dir);
    if (!fs.existsSync(d)) continue;
    for (const f of fs.readdirSync(d)) if (f.endsWith('.tau')) files.push(path.join(d, f));
  }
  assert(files.length >= 10, `found only ${files.length} programs`);
  const g = await registry.loadGrammar('source.tau');
  for (const f of files) {
    let stack = vsctm.INITIAL;
    for (const line of fs.readFileSync(f, 'utf8').split('\n')) stack = g.tokenizeLine(line, stack).ruleStack;
    // the stack of a finished file holds only the root rule
    assert.strictEqual(stack.depth, 1, `${path.basename(f)} ends inside a construct (stack depth ${stack.depth})`);
  }
});

(async () => {
  let failed = 0;
  for (const [name, fn] of cases) {
    try {
      await fn();
      console.log(`ok   ${name}`);
    } catch (e) {
      failed++;
      console.log(`FAIL ${name}\n     ${String(e.message).split('\n').join('\n     ')}`);
    }
  }
  console.log(`${cases.length - failed}/${cases.length} grammar tests passed`);
  process.exit(failed ? 1 : 0);
})();
