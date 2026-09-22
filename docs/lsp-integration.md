# Remote Language Server Protocol (LSP) Offloading

Farhand allows developers on weak, memory-constrained, or battery-limited machines to offload resource-intensive Language Servers (such as `rust-analyzer`, `pyright`, `gopls`, `clangd`, and `typescript-language-server`) to a powerful remote agent.

Your code editor runs locally with full responsiveness, while indexing, AST parsing, type inference, macro expansions, and compiler diagnostics run remotely on the agent host.

---

## 1. How It Works

```
┌─────────────────────────────────┐                    ┌─────────────────────────────────┐
│         Local Machine           │                    │       Remote Agent Daemon       │
│                                 │                    │                                 │
│  ┌───────────────────────────┐  │                    │  ┌───────────────────────────┐  │
│  │   Code Editor (VS Code,   │  │                    │  │   Language Server (e.g.   │  │
│  │   Neovim, Helix, Zed)     │  │                    │  │   rust-analyzer, pyright) │  │
│  └─────────────┬─────────────┘  │                    │  └─────────────▲─────────────┘  │
│                │ stdio (JSON-RPC)                    │                │ stdio          │
│  ┌─────────────▼─────────────┐  │                    │  ┌─────────────┴─────────────┐  │
│  │     fh lsp offloader      │  │    Raw / TLS TCP   │  │            fhd            │  │
│  │ - Initial delta sync      │◄─┼────────────────────┼─►│ - Persistent workspace    │  │
│  │ - Bidirectional URI map   │  │   Framed Socket    │  │ - Process group isolation │  │
│  │ - didSave background sync │  │                    │  │ - Raw chunked streaming   │  │
│  └───────────────────────────┘  │                    │  └───────────────────────────┘  │
└─────────────────────────────────┘                    └─────────────────────────────────┘
```

1. **Initial Workspace Sync**:
   When launched by your editor, `fh lsp` performs a delta sync so the remote workspace matches your local project directory.
2. **Bidirectional JSON-RPC Path Translation**:
   Language servers communicate via JSON-RPC using `file://` URIs:
   - **Editor → Remote**: Local file URIs (e.g. `file:///home/user/project/src/main.rs`) are dynamically translated to remote workspace URIs (e.g. `file:///var/farhand/workspaces/project/src/main.rs`).
   - **Remote → Editor**: Diagnostic reports, definition locations, and completion items from the remote language server are translated back to your local filesystem URIs.
3. **On-Save Synchronization (`textDocument/didSave`)**:
   Whenever you save a file (`Ctrl+S`), `fh lsp` inspects the JSON-RPC event stream and automatically pushes the modified file delta to the remote workspace in the background. The remote language server updates its compiler cache and publishes updated diagnostics immediately.
4. **Lifecycle & Cleanup**:
   When your editor closes or restarts the language server, `fh lsp` closes the connection, and `fhd` cleanly terminates the remote process group without leaving orphan compiler processes.

---

## 2. Server-Side Preparation

The remote agent machine must have the desired language server binary installed on its `$PATH`.

### Rust (`rust-analyzer`)
On the remote Mac or Linux machine:
```bash
# Via rustup (recommended)
rustup component add rust-analyzer

# Or via Homebrew (macOS)
brew install rust-analyzer
```

### Go (`gopls`)
```bash
go install golang.org/x/tools/gopls@latest
```

### Python (`pyright` or `pylsp`)
```bash
npm install -g pyright
# Or: pip install python-lsp-server
```

### C / C++ (`clangd`)
```bash
# macOS
brew install llvm
# Linux
sudo apt-get install clangd
```

> **Note for `launchd` / `systemd` Daemons**: Ensure the directory containing your language server (e.g. `/opt/homebrew/bin` or `$HOME/.cargo/bin`) is included in the `PATH` environment variable of your `fhd` service definition.

---

## 3. Editor Setup & Configuration

LSP clients communicate over standard I/O (`stdin` / `stdout`). Follow the configuration for your editor below:

### 1. VS Code / Cursor

VS Code's language extensions (such as `rust-analyzer` or `gopls`) expect an executable path for their server binary.

#### Step 1: Create a wrapper script
Create a small wrapper script on your local `$PATH` (e.g. `~/.cargo/bin/fh-rust-analyzer`):

```bash
cat << 'EOF' > ~/.cargo/bin/fh-rust-analyzer
#!/bin/sh
exec fh lsp -- rust-analyzer "$@"
EOF
chmod +x ~/.cargo/bin/fh-rust-analyzer
```

#### Step 2: Configure `.vscode/settings.json`
In your project's `.vscode/settings.json` (or User Settings):

```json
{
  "rust-analyzer.server.path": "fh-rust-analyzer"
}
```

For **Python (Pyright)**:
```json
{
  "python.languageServer": "Default",
  "basedpyright.serverPath": "fh-pyright"
}
```
*(With `fh-pyright` containing `exec fh lsp -- pyright-langserver --stdio "$@"`, executable)*

#### Step 3: Restart the Language Server
In VS Code, press `Ctrl+Shift+P` (or `Cmd+Shift+P` on macOS) and run:
`> Rust Analyzer: Restart server`

---

### 2. Neovim (`nvim-lspconfig`)

In Neovim, you can pass command arguments directly in your LSP setup configuration:

```lua
-- ~/.config/nvim/lua/plugins/lsp.lua (or init.lua)
local lspconfig = require('lspconfig')

-- Rust (rust-analyzer)
lspconfig.rust_analyzer.setup({
  cmd = { "fh", "lsp", "--", "rust-analyzer" },
  root_dir = lspconfig.util.root_pattern("Cargo.toml", ".git"),
})

-- Go (gopls)
lspconfig.gopls.setup({
  cmd = { "fh", "lsp", "--", "gopls" },
  root_dir = lspconfig.util.root_pattern("go.mod", ".git"),
})

-- Python (pyright)
lspconfig.pyright.setup({
  cmd = { "fh", "lsp", "--", "pyright-langserver", "--stdio" },
  root_dir = lspconfig.util.root_pattern("pyproject.toml", "setup.py", ".git"),
})

-- C / C++ (clangd)
lspconfig.clangd.setup({
  cmd = { "fh", "lsp", "--", "clangd" },
  root_dir = lspconfig.util.root_pattern("compile_commands.json", ".git"),
})
```

---

### 3. Helix Editor

Configure `~/.config/helix/languages.toml` (or project `.helix/languages.toml`):

```toml
[language-server.remote-rust-analyzer]
command = "fh"
args = ["lsp", "--", "rust-analyzer"]

[[language]]
name = "rust"
language-servers = ["remote-rust-analyzer"]

[language-server.remote-gopls]
command = "fh"
args = ["lsp", "--", "gopls"]

[[language]]
name = "go"
language-servers = ["remote-gopls"]

[language-server.remote-pyright]
command = "fh"
args = ["lsp", "--", "pyright-langserver", "--stdio"]

[[language]]
name = "python"
language-servers = ["remote-pyright"]
```

---

### 4. Zed Editor

In `~/.config/zed/settings.json`:

```json
{
  "lsp": {
    "rust-analyzer": {
      "binary": {
        "path": "fh",
        "arguments": ["lsp", "--", "rust-analyzer"]
      }
    },
    "gopls": {
      "binary": {
        "path": "fh",
        "arguments": ["lsp", "--", "gopls"]
      }
    }
  }
}
```

---

### 5. Emacs (`eglot` & `lsp-mode`)

#### Using `eglot` (Built-in Emacs 29+)
```elisp
(with-eval-after-load 'eglot
  (add-to-list 'eglot-server-programs
               '(rust-mode . ("fh" "lsp" "--" "rust-analyzer")))
  (add-to-list 'eglot-server-programs
               '(go-mode . ("fh" "lsp" "--" "gopls"))))
```

#### Using `lsp-mode`
```elisp
(with-eval-after-load 'lsp-rust
  (setq lsp-rust-analyzer-server-command '("fh" "lsp" "--" "rust-analyzer")))
```

---

## 4. Command-Line Reference

```bash
fh lsp [OPTIONS] -- <COMMAND>...
```

### Options:
- `--agent <HOST:PORT>`: Target a specific agent (defaults to `FARHAND_HOST` or `.farhand.yaml`).
- `--no-sync`: Bypass the initial project scan and delta upload upon connecting.
- `--no-save-sync`: Disable automatic background file sync when `textDocument/didSave` is fired by your editor.
- `--tls`: Enable TLS connection to the remote agent.
- `--tls-fingerprint <HASH>`: SHA-256 fingerprint for certificate pinning.

---

## 5. Troubleshooting & Diagnostics

### 1. `fh lsp -- rust-analyzer` shows no output in the terminal
This is expected behavior. LSP communicates via JSON-RPC. When run directly in a terminal without an editor piping `stdin`, the server idles waiting for the editor's `initialize` packet.

### 2. `/bin/sh: rust-analyzer: command not found`
The language server binary is not accessible on the remote agent's `$PATH`.
- Check where the binary is installed remotely (`which rust-analyzer`).
- If installed via Homebrew or Cargo, specify the full path:
  ```bash
  fh lsp -- /opt/homebrew/bin/rust-analyzer
  # Or:
  fh lsp -- /Users/builder/.cargo/bin/rust-analyzer
  ```
- Alternatively, update the `PATH` key in your agent's `/Library/LaunchDaemons/com.farhand.fhd.plist` or `systemd` service file.

### 3. Diagnostics not updating on save
Ensure `--no-save-sync` is not passed. Farhand automatically intercepts `textDocument/didSave` events and uploads only the saved file to the remote workspace. Check that the saved file is not in `.farhandignore` or `.gitignore`.
