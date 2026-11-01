// Regenerates assets/fonts/*.ttf from the SAME @fontsource files SONIX bundles (latin subsets,
// weights 400/500/600/700): WOFF1 -> sfnt (zlib per table). The previous TTFs came from the GPUI
// spike and had different Space Grotesk advances than the web app's faces (parity runner finding).
// Usage: node scripts/gen-fonts.mjs <SoniX checkout>   (needs `npm ci` there)
import fs from "node:fs";
import zlib from "node:zlib";
import path from "node:path";
const root = process.argv[2];
if (!root) throw new Error("usage: node scripts/gen-fonts.mjs <SoniX checkout>");
const out = path.join(path.dirname(new URL(import.meta.url).pathname.replace(/^\//, "")), "..", "assets", "fonts");
for (const fam of ["space-grotesk", "inter", "jetbrains-mono"]) {
  for (const w of [400, 500, 600, 700]) {
    const src = path.join(root, "node_modules", "@fontsource", fam, "files", `${fam}-latin-${w}-normal.woff`);
    const b = fs.readFileSync(src);
    if (b.toString("ascii", 0, 4) !== "wOFF") throw new Error("not WOFF1: " + src);
    const flavor = b.readUInt32BE(4), n = b.readUInt16BE(12);
    const tabs = [];
    for (let i = 0; i < n; i++) {
      const o = 44 + i * 20;
      const tag = b.toString("ascii", o, o + 4), off = b.readUInt32BE(o + 4), comp = b.readUInt32BE(o + 8), orig = b.readUInt32BE(o + 12), sum = b.readUInt32BE(o + 16);
      let data = b.subarray(off, off + comp);
      if (comp < orig) data = zlib.inflateSync(data);
      tabs.push({ tag, data, sum });
    }
    tabs.sort((a, c) => (a.tag < c.tag ? -1 : 1));
    const hdr = Buffer.alloc(12 + 16 * n);
    hdr.writeUInt32BE(flavor, 0);
    hdr.writeUInt16BE(n, 4);
    const sr = 2 ** Math.floor(Math.log2(n)) * 16;
    hdr.writeUInt16BE(sr, 6);
    hdr.writeUInt16BE(Math.floor(Math.log2(n)), 8);
    hdr.writeUInt16BE(n * 16 - sr, 10);
    let off = hdr.length;
    const parts = [hdr];
    tabs.forEach((t, i) => {
      const o = 12 + i * 16;
      hdr.write(t.tag, o, "ascii");
      hdr.writeUInt32BE(t.sum, o + 4);
      hdr.writeUInt32BE(off, o + 8);
      hdr.writeUInt32BE(t.data.length, o + 12);
      parts.push(t.data);
      const pad = (4 - (t.data.length % 4)) % 4;
      if (pad) parts.push(Buffer.alloc(pad));
      off += t.data.length + pad;
    });
    fs.writeFileSync(path.join(out, `${fam}-${w}.ttf`), Buffer.concat(parts));
    console.log("wrote", `${fam}-${w}.ttf`);
  }
}
