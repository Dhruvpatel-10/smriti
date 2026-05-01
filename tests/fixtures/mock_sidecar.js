#!/usr/bin/env node
// Mock JSON-RPC sidecar for integration tests.
// Reads one JSON-RPC request per line from stdin, writes one JSON-RPC response
// per line to stdout. Sidecar errors go to stderr.
//
// MOCK_BEHAVIOR env var controls special modes:
//   crash_after_n=N  — exit(1) after N successful requests
//   slow=MS          — delay every response by MS milliseconds
//   hang             — never respond (useful for timeout tests)

'use strict';

const readline = require('readline');

const behavior = process.env.MOCK_BEHAVIOR || '';
let crashAfter = null;
let slowMs = 0;
let hang = false;
let requestCount = 0;

if (behavior.startsWith('crash_after_n=')) {
  crashAfter = parseInt(behavior.split('=')[1], 10);
} else if (behavior.startsWith('slow=')) {
  slowMs = parseInt(behavior.split('=')[1], 10);
} else if (behavior === 'hang') {
  hang = true;
}

const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });

function respond(id, result) {
  const msg = JSON.stringify({ jsonrpc: '2.0', id, result });
  process.stdout.write(msg + '\n');
}

function respondError(id, code, message) {
  const msg = JSON.stringify({ jsonrpc: '2.0', id, error: { code, message } });
  process.stdout.write(msg + '\n');
}

async function delay(ms) {
  return new Promise(resolve => setTimeout(resolve, ms));
}

async function handleRequest(line) {
  let req;
  try {
    req = JSON.parse(line);
  } catch (e) {
    process.stderr.write(`bad JSON: ${e.message}\n`);
    return;
  }

  const { id, method, params } = req;

  if (hang) {
    // Never respond — caller will time out.
    return;
  }

  if (slowMs > 0) {
    await delay(slowMs);
  }

  requestCount++;

  if (method === 'health') {
    respond(id, { ok: true, version: 'mock-1.0.0' });
  } else if (method === 'extract_relations') {
    const memId = (params && params.memory_id) ? params.memory_id : 'memory:unknown';
    respond(id, {
      relations: [
        {
          from: memId,
          to: 'memory:test_target',
          kind: 'mentions',
          confidence: 0.85,
        },
      ],
    });
  } else {
    respondError(id, -32601, `method not found: ${method}`);
  }

  if (crashAfter !== null && requestCount >= crashAfter) {
    process.stderr.write(`mock_sidecar: crashing after ${requestCount} requests\n`);
    process.exit(1);
  }
}

rl.on('line', line => {
  if (line.trim() === '') return;
  handleRequest(line).catch(e => {
    process.stderr.write(`handleRequest error: ${e.message}\n`);
  });
});

rl.on('close', () => {
  process.exit(0);
});
