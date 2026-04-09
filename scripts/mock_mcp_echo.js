#!/usr/bin/env node

const tools = [
  {
    name: "echo",
    description: "Return the provided arguments as structured content."
  }
];

function writeMessage(obj) {
  const body = Buffer.from(JSON.stringify(obj), "utf8");
  process.stdout.write(`Content-Length: ${body.length}\r\n\r\n`);
  process.stdout.write(body);
}

let buffer = Buffer.alloc(0);

process.stdin.on("data", (chunk) => {
  buffer = Buffer.concat([buffer, chunk]);

  while (true) {
    const marker = buffer.indexOf("\r\n\r\n");
    if (marker === -1) {
      return;
    }

    const header = buffer.slice(0, marker).toString("utf8");
    const match = header.match(/Content-Length:\s*(\d+)/i);
    if (!match) {
      return;
    }

    const contentLength = Number(match[1]);
    const totalLength = marker + 4 + contentLength;
    if (buffer.length < totalLength) {
      return;
    }

    const body = buffer.slice(marker + 4, totalLength).toString("utf8");
    buffer = buffer.slice(totalLength);
    const message = JSON.parse(body);
    handleMessage(message);
  }
});

function handleMessage(message) {
  if (message.method === "initialize") {
    writeMessage({
      jsonrpc: "2.0",
      id: message.id,
      result: {
        protocolVersion: "2025-03-26",
        serverInfo: { name: "mock-echo", version: "0.1.0" },
        capabilities: { tools: {} }
      }
    });
    return;
  }

  if (message.method === "tools/list") {
    writeMessage({
      jsonrpc: "2.0",
      id: message.id,
      result: { tools }
    });
    return;
  }

  if (message.method === "tools/call") {
    const args = message.params?.arguments ?? {};
    writeMessage({
      jsonrpc: "2.0",
      id: message.id,
      result: {
        content: [
          {
            type: "text",
            text: JSON.stringify(args)
          }
        ],
        isError: false
      }
    });
    return;
  }

  writeMessage({
    jsonrpc: "2.0",
    id: message.id,
    error: { code: -32601, message: "Method not found" }
  });
}

