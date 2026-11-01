// Makes the bundled TTFs resolvable by weight. @fontsource's static instances name the 500/600 faces "<Family> Medium" /
// "<Family> SemiBold" (legacy nameID 1, subfamily "Regular"), so DirectWrite files them under *separate* families and a
// request for `Space Grotesk` at weight 600 silently gets the Bold face (500 gets Regular). Chromium reads the weight from
// the @font-face rule instead. This rewrites nameID 1 to the plain family and nameID 2 to Regular/Bold, drops the
// typographic/WWS names (16, 17, 21, 22) and keeps OS/2 usWeightClass, so one family holds 400/500/600/700.
// Idempotent. Usage: node scripts/fix-font-families.mjs   (run after scripts/gen-fonts.mjs)
import fs from "node:fs";
import path from "node:path";
const dir = path.join(path.dirname(new URL(import.meta.url).pathname.replace(/^\//, "")), "..", "assets", "fonts");
const FAMILY = { "space-grotesk": "Space Grotesk", inter: "Inter", "jetbrains-mono": "JetBrains Mono" };

function checksum(buf) {
  let s = 0;
  const padded = Buffer.concat([buf, Buffer.alloc((4 - (buf.length % 4)) % 4)]);
  for (let i = 0; i < padded.length; i += 4) s = (s + padded.readUInt32BE(i)) >>> 0;
  return s;
}

function rebuildName(old, family, bold) {
  const count = old.readUInt16BE(2), so = old.readUInt16BE(4);
  const recs = [];
  for (let i = 0; i < count; i++) {
    const r = 6 + i * 12;
    const rec = { pid: old.readUInt16BE(r), eid: old.readUInt16BE(r + 2), lid: old.readUInt16BE(r + 4), id: old.readUInt16BE(r + 6) };
    rec.data = old.subarray(so + old.readUInt16BE(r + 10), so + old.readUInt16BE(r + 10) + old.readUInt16BE(r + 8));
    recs.push(rec);
  }
  const enc = (rec, s) => (rec.pid === 3 || rec.pid === 0 ? Buffer.from(s, "utf16le").swap16() : Buffer.from(s, "latin1"));
  const kept = recs.filter((r) => ![16, 17, 21, 22].includes(r.id));
  for (const r of kept) {
    if (r.id === 1) r.data = enc(r, family);
    if (r.id === 2) r.data = enc(r, bold ? "Bold" : "Regular");
  }
  kept.sort((a, b) => a.pid - b.pid || a.eid - b.eid || a.lid - b.lid || a.id - b.id);
  const strOff = 6 + kept.length * 12;
  const hdr = Buffer.alloc(strOff);
  hdr.writeUInt16BE(0, 0);
  hdr.writeUInt16BE(kept.length, 2);
  hdr.writeUInt16BE(strOff, 4);
  let o = 0;
  const strs = [];
  kept.forEach((r, i) => {
    const p = 6 + i * 12;
    hdr.writeUInt16BE(r.pid, p);
    hdr.writeUInt16BE(r.eid, p + 2);
    hdr.writeUInt16BE(r.lid, p + 4);
    hdr.writeUInt16BE(r.id, p + 6);
    hdr.writeUInt16BE(r.data.length, p + 8);
    hdr.writeUInt16BE(o, p + 10);
    strs.push(r.data);
    o += r.data.length;
  });
  return Buffer.concat([hdr, ...strs]);
}

for (const f of fs.readdirSync(dir).filter((f) => f.endsWith(".ttf"))) {
  const m = f.match(/^(.*)-(\d+)\.ttf$/);
  const b = fs.readFileSync(path.join(dir, f));
  const n = b.readUInt16BE(4);
  const tabs = [];
  for (let i = 0; i < n; i++) {
    const e = 12 + i * 16;
    const off = b.readUInt32BE(e + 8), len = b.readUInt32BE(e + 12);
    tabs.push({ tag: b.toString("ascii", e, e + 4), data: b.subarray(off, off + len) });
  }
  // STAT carries a `wght` axis value only for some instances (the SemiBold one has none) and DirectWrite then misreads the
  // face's weight (it resolved 600 to Bold); the weight comes from OS/2 usWeightClass alone.
  const keep = tabs.filter((t) => t.tag !== "STAT");
  tabs.length = 0;
  tabs.push(...keep);
  const nm = tabs.find((t) => t.tag === "name");
  nm.data = rebuildName(nm.data, FAMILY[m[1]], m[2] === "700");
  const hdr = Buffer.from(b.subarray(0, 12 + 16 * tabs.length));
  hdr.writeUInt16BE(tabs.length, 4);
  const sr = 2 ** Math.floor(Math.log2(tabs.length)) * 16;
  hdr.writeUInt16BE(sr, 6);
  hdr.writeUInt16BE(Math.floor(Math.log2(tabs.length)), 8);
  hdr.writeUInt16BE(tabs.length * 16 - sr, 10);
  let off = hdr.length;
  const parts = [hdr];
  tabs.forEach((t, i) => {
    const e = 12 + i * 16;
    hdr.write(t.tag, e, "ascii");
    hdr.writeUInt32BE(checksum(t.data), e + 4);
    hdr.writeUInt32BE(off, e + 8);
    hdr.writeUInt32BE(t.data.length, e + 12);
    parts.push(t.data);
    const pad = (4 - (t.data.length % 4)) % 4;
    if (pad) parts.push(Buffer.alloc(pad));
    off += t.data.length + pad;
  });
  fs.writeFileSync(path.join(dir, f), Buffer.concat(parts));
  console.log("fixed", f);
}
