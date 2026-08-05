// Test-only bootstrap: loads the extension indicated by EXTENSION_PATH.
// Not proprietary — this is the contract we interop against, rewritten.
import { pathToFileURL } from "node:url";
const entry = process.env.EXTENSION_PATH;
if (!entry) {
  console.error("[xli-test-bootstrap] EXTENSION_PATH missing");
  process.exit(1);
}
await import(pathToFileURL(entry).href);
