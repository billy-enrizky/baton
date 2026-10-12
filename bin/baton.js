#!/usr/bin/env node
const originalEmitWarning = process.emitWarning;
process.emitWarning = function (warning, ...args) {
  const text = typeof warning === "string" ? warning : warning?.message ?? "";
  if (text.includes("SQLite is an experimental feature")) return;
  return originalEmitWarning.call(process, warning, ...args);
};
await import(new URL("../dist/bin.js", import.meta.url));
