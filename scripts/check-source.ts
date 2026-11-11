import { readdir } from "node:fs/promises";
import { join, extname } from "node:path";

const excluded: Record<string, true> = {
  ".git": true,
  target: true,
  node_modules: true,
  ".direnv": true,
  ".data": true,
};
const rejected: string[] = [];

async function check(directory: string): Promise<void> {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      // Zed downloads third-party grammar sources into this ignored directory.
      if (path !== "grammars" && !excluded[entry.name]) await check(path);
    } else if ([".js", ".jsx", ".mjs", ".cjs"].includes(extname(entry.name))) {
      rejected.push(path);
    }
  }
}

await check(".");
if (rejected.length) {
  console.error("JavaScript source is not permitted. Use TypeScript:");
  for (const path of rejected) console.error(path);
  process.exit(1);
}
console.log("Source check passed: no JavaScript source files.");
