#!/usr/bin/env node
// Install the mcp-skills bridge extension into the user-level extension dir so
// every project shares one implementation (a junction, not a copy).
//
//   node tools/install_pi_extension.mjs            # install / refresh
//   node tools/install_pi_extension.mjs --uninstall
import { existsSync, mkdirSync, rmSync, symlinkSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const src = resolve(here, "..", "pi-extensions", "mcp-skills");
const destRoot = join(homedir(), ".pi", "agent", "extensions");
const dest = join(destRoot, "mcp-skills");

if (!existsSync(src)) {
  console.error(`找不到扩展源目录: ${src}`);
  process.exit(1);
}

if (process.argv.includes("--uninstall")) {
  if (existsSync(dest)) rmSync(dest, { recursive: true, force: true });
  console.log(`removed  ${dest}`);
  process.exit(0);
}

mkdirSync(destRoot, { recursive: true });
if (existsSync(dest)) rmSync(dest, { recursive: true, force: true });
symlinkSync(src, dest, "junction");
console.log(`junction  ${dest}`);
console.log(`       -> ${src}`);
console.log("重启 pi（或 /reload）后生效；所有项目共用同一份实现。");
