# smriti-sidecar

Bridges smriti (Rust MCP server) to the Claude Agent SDK via JSON-RPC 2.0 over stdio.
Smriti spawns this as a subprocess; stdout carries the protocol, stderr carries logs.

## Setup

```sh
npm install        # or: pnpm install
npm run build      # compiles src/ → dist/
```

## Usage

Run directly (production):
```sh
npm run start
```

Dev (no compile step, uses tsx):
```sh
npm run dev
```

## Test

Health check:
```sh
echo '{"jsonrpc":"2.0","id":1,"method":"health","params":{}}' | node dist/index.js
```

Unknown method (expect -32601):
```sh
echo '{"jsonrpc":"2.0","id":2,"method":"unknown","params":{}}' | node dist/index.js
```

## Auth

Uses Claude Max plan via the `claude` CLI's keychain OAuth — no API key required.
Ensure you are authenticated: `claude auth login`.

## Protocol

Line-delimited JSON-RPC 2.0. Each line in → each line out (no response for notifications).

| Method | Status |
|---|---|
| `health` | Implemented |
| `extract_relations` | Stubbed (P1-D) |

## ADR

See ADR-013 (to be written) for full rationale on the sidecar architecture.
