# Tau for VS Code

Language support for [Tau](https://github.com/pigeon318/Tau), a maths and science programming language.

- **Live errors.** The compiler checks the file as you type (types and the borrow checker) and the errors appear as squiggles, with the compiler's hints.
- **Hover types.** Hover a variable, parameter, field, method or function to see its inferred type or signature.
- **Run and test.** A `Run` link above `fn main`, a `Run test` link above every `#[test]`, `Ctrl+F5` to run the file, and commands for the interpreter, building an executable and viewing the generated C.
- **Highlighting, snippets, outline, brackets, comment toggling** (nested block comments, string interpolation `"{x}"`, number suffixes, `par`/`spawn`).

## Setup

The extension talks to the `tau` command, so install the compiler first and make sure `tau --version` works in a terminal.
If `tau` is not on your `PATH`, set **Tau: Path** in the settings to the full path (on Windows, the path of `tau.exe`).

Running compiled programs needs a C compiler (clang or gcc; on Windows, MinGW-w64 gcc). Without one, `Run` still works: the program runs on the interpreter.

## Settings

| Setting | Default | |
| --- | --- | --- |
| `tau.path` | `tau` | the compiler |
| `tau.checkOnType` | `true` | check while typing, not only on save |
| `tau.checkDelayMs` | `350` | pause after the last keystroke before checking |
| `tau.codeLens` | `true` | Run / Run test links |
| `tau.hover` | `true` | types on hover |
| `tau.runInTerminal` | `true` | run in the integrated terminal (needed for programs that read the keyboard) |
| `tau.threads` | none | sets `TAU_THREADS` for programs run from the editor |

## Building and installing from source

```bash
cd editors/vscode
npm install
npm test          # grammar tests and extension tests (needs `cargo build` first)
npm run package   # produces tau-lang-<version>.vsix
code --install-extension tau-lang-0.1.0.vsix
```

## How it works

For each check the extension runs `tau check - --json` with the file's text on standard input, so unsaved changes are checked too.
The compiler answers with diagnostics (0-based lines, UTF-16 columns, ready for the editor) and a list of hover texts.
The same data will be served by a language server later, for other editors and for Tau's own IDE.
