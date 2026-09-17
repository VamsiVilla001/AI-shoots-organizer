#!/usr/bin/env node
/**
 * Packages apps/premiere-panel into the .ccx that the desktop installer ships
 * and installs on first launch (see apps/desktop/src-tauri/src/premiere_plugin.rs).
 *
 * A .ccx is an ordinary zip with manifest.json at its root, so this builds one
 * without any Adobe tooling — which matters because the panel has to be
 * packageable on a CI runner and on a machine that has never seen the UXP
 * Developer Tool.
 *
 * The zip is written here rather than shelled out to, because the two obvious
 * shortcuts are both wrong: `Compress-Archive` on Windows PowerShell 5.1 writes
 * `\` path separators, which the zip spec forbids and non-Windows readers
 * mis-parse, and `zip` is not on Windows at all.
 *
 * What this cannot do is sign it. UDT's own Package command produces a
 * self-signed .ccx; if a Creative Cloud version turns out to refuse the
 * unsigned one, package it there once and stage it here instead:
 *
 *     node scripts/package-premiere-panel.mjs --from path/to/signed.ccx
 *
 * Everything downstream — the bundle, the installer, the first-launch install —
 * treats either the same way, because both are just a file at the staged path.
 */
import { deflateRawSync } from 'node:zlib';
import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const panelDir = join(repoRoot, 'apps', 'premiere-panel');
const stagedDir = join(repoRoot, 'apps', 'desktop', 'src-tauri', 'resources', 'premiere-panel');
const stagedCcx = join(stagedDir, 'skwad-collections.ccx');
/**
 * The panel's manifest, staged beside the package. The app reads the version
 * from here to decide whether the installed panel is current — comparing a
 * manifest against a manifest, rather than against the app's own version,
 * which the panel is free not to share.
 */
const stagedManifest = join(stagedDir, 'manifest.json');

/**
 * Only what the panel actually loads at runtime. README.md and test-bridge.html
 * are development aids — shipping them would put a page that talks to the
 * bridge inside every editor's Premiere install for no reason.
 */
const CONTENTS = ['manifest.json', 'index.html', 'main.js', 'fonts'];

function fail(message) {
  console.error(`package-premiere-panel: ${message}`);
  process.exit(1);
}

/** Every file under `dir`, as archive-relative paths with `/` separators. */
function walk(dir, prefix = '') {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
    const name = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) out.push(...walk(join(dir, entry.name), name));
    else out.push(name);
  }
  return out;
}

const CRC_TABLE = (() => {
  const table = new Int32Array(256);
  for (let i = 0; i < 256; i += 1) {
    let c = i;
    for (let k = 0; k < 8; k += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[i] = c;
  }
  return table;
})();

function crc32(buf) {
  let c = -1;
  for (let i = 0; i < buf.length; i += 1) c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
}

/**
 * A minimal zip writer: one deflated entry per file, then the central
 * directory. No zip64 — the panel is a few hundred KB and nowhere near any of
 * the 32-bit limits that would require it.
 */
function writeZip(files, outFile) {
  const locals = [];
  const central = [];
  let offset = 0;

  for (const { name, data } of files) {
    const nameBuf = Buffer.from(name, 'utf8');
    const compressed = deflateRawSync(data);
    const crc = crc32(data);

    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4); // version needed
    local.writeUInt16LE(0x0800, 6); // UTF-8 names
    local.writeUInt16LE(8, 8); // deflate
    local.writeUInt16LE(0, 10); // mod time — fixed, so the build is reproducible
    local.writeUInt16LE(0x21, 12); // mod date — 1980-01-01, the zip epoch
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(compressed.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(nameBuf.length, 26);
    locals.push(local, nameBuf, compressed);

    const entry = Buffer.alloc(46);
    entry.writeUInt32LE(0x02014b50, 0);
    entry.writeUInt16LE(20, 4); // version made by
    entry.writeUInt16LE(20, 6);
    entry.writeUInt16LE(0x0800, 8);
    entry.writeUInt16LE(8, 10);
    entry.writeUInt16LE(0, 12);
    entry.writeUInt16LE(0x21, 14);
    entry.writeUInt32LE(crc, 16);
    entry.writeUInt32LE(compressed.length, 20);
    entry.writeUInt32LE(data.length, 24);
    entry.writeUInt16LE(nameBuf.length, 28);
    entry.writeUInt32LE((0o100644 << 16) >>> 0, 38); // external attrs: a regular file
    entry.writeUInt32LE(offset, 42);
    central.push(entry, nameBuf);

    offset += 30 + nameBuf.length + compressed.length;
  }

  const centralBuf = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(files.length, 8);
  end.writeUInt16LE(files.length, 10);
  end.writeUInt32LE(centralBuf.length, 12);
  end.writeUInt32LE(offset, 16);

  writeFileSync(outFile, Buffer.concat([...locals, centralBuf, end]));
}

const fromIndex = process.argv.indexOf('--from');
mkdirSync(dirname(stagedCcx), { recursive: true });

if (fromIndex !== -1) {
  const signed = resolve(process.argv[fromIndex + 1] ?? '');
  if (!signed || !existsSync(signed)) fail('--from needs the path of an existing .ccx');
  copyFileSync(signed, stagedCcx);
  // The sidecar still comes from source: --from expects a package built from
  // this same panel, just signed by the UXP Developer Tool.
  copyFileSync(join(panelDir, 'manifest.json'), stagedManifest);
  console.log(`package-premiere-panel: staged the signed package from ${signed}`);
} else {
  const manifestPath = join(panelDir, 'manifest.json');
  if (!existsSync(manifestPath)) fail(`no manifest.json in ${panelDir}`);
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));

  const files = [];
  for (const entry of CONTENTS) {
    const source = join(panelDir, entry);
    if (!existsSync(source)) fail(`the panel is missing ${entry}`);
    const names = statSync(source).isDirectory() ? walk(source, entry) : [entry];
    for (const name of names) files.push({ name, data: readFileSync(join(panelDir, name)) });
  }
  writeZip(files, stagedCcx);
  copyFileSync(manifestPath, stagedManifest);
  console.log(`package-premiere-panel: built ${manifest.id} ${manifest.version} (unsigned, ${files.length} files)`);
}

console.log(`package-premiere-panel: ${stagedCcx} (${(statSync(stagedCcx).size / 1024).toFixed(0)} KB)`);
