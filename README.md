<p align="center">
  <img src="assets/icon.png" width="112" height="112" alt="Babbage" />
</p>

<h1 align="center">Babbage</h1>

<p align="center">
  <b>Blazing-fast local file and content search. Private, offline, and lightweight.</b>
</p>

<p align="center">
  Search file names across your drives in milliseconds.<br/>
  Search inside documents, images, and code. 100% local — nothing leaves your machine.
</p>

<p align="center">
  <a href="LICENSE"><img alt="MIT license" src="https://img.shields.io/badge/license-MIT-111111?style=for-the-badge" /></a>
  <img alt="Offline & Local" src="https://img.shields.io/badge/privacy-100%25%20local-success?style=for-the-badge" />
</p>

---

## Find it the way you remember it

You don't remember the filename. You remember what was in it.

- The PDF about database indexing, saved as `notes-final-2.pdf`.
- The log line from a failed deploy, somewhere in a folder of logs.
- The screenshot with a CUDA error, which has no descriptive name at all.

Type what you remember. Babbage searches names, text inside files, and meaning, displaying matches with keywords highlighted.

## Features

### Fast & Lightweight
- Millisecond search across thousands of files on a warm cache.
- Results update instantly as you type.
- Incremental indexing: changed files are updated without rescanning your entire disk.
- Runs discreetly in your system tray; summons instantly with a keyboard shortcut.

### Deep Document & Image Search
- **Documents & Office**: PDF, DOCX, PPTX, XLSX, EPUB, ODT, ODS, ODP, RTF, CSV, and plain text.
- **Code & Developer Formats**: Rust, Python, TypeScript, JavaScript, Go, C/C++, Java, Shell, SQL, JSON, YAML, TOML, Dockerfiles, and 50+ languages.
- **Images & OCR**: On-device optical character recognition for screenshots, scanned documents, and photos.
- **Multilingual Support**: Custom tokenizers for Latin, Chinese (Hanzi), Japanese (Kanji/Kana), and Korean (Hangul).

### Private & Local
- **No telemetry, no tracking, no phone-home.**
- Models run purely on-device using local ONNX inference.
- Sensitive files automatically excluded (`.ssh`, `.gnupg`, `.aws`, `.env`, certificates, and hidden credentials).

### AI Agent Support (MCP)
- Direct Model Context Protocol (MCP) server support for AI developer agents like Claude Code and Cursor.
- Provides read-only, fast, local filesystem retrieval.

```json
{
  "mcpServers": {
    "babbage": { "command": "babbage-cli", "args": ["mcp"] }
  }
}
```

---

## Building from Source

### Prerequisites
- [Node.js](https://nodejs.org/) (v18+) & `npm`
- [Rust](https://rustup.rs/) (latest stable)

### Build & Run

```bash
# Install frontend dependencies
npm install

# Run in development mode
npm run dev

# Or run with Tauri desktop window
npm run tauri dev

# Package release installer (.exe / .msi)
npm run tauri build
```

---

## CLI Usage

`babbage-cli` provides scriptable and headless access to the local search index:

```bash
babbage-cli search "docker error"
babbage-cli search "invoice" --ext pdf --since 30d
babbage-cli search "resume" --json
babbage-cli state
babbage-cli mcp
```

---

## License

MIT License. See [LICENSE](LICENSE) for details.
