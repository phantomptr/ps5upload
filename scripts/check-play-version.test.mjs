import { test } from "node:test";
import assert from "node:assert/strict";
import zlib from "node:zlib";

import { paramProblems, ps5ContentVersion, zipEntries, zipProblems, zipRead } from "./check-play-version.mjs";

/** A zip of `files` ({name: string|Buffer}); deflated when `deflate`. */
function makeZip(files, deflate = false) {
  const locals = [];
  const centrals = [];
  let offset = 0;
  for (const [name, body] of Object.entries(files)) {
    const raw = Buffer.from(body);
    const data = deflate ? zlib.deflateRawSync(raw) : raw;
    const nameBuf = Buffer.from(name);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(deflate ? 8 : 0, 8);
    local.writeUInt32LE(data.length, 18);
    local.writeUInt32LE(raw.length, 22);
    local.writeUInt16LE(nameBuf.length, 26);
    locals.push(local, nameBuf, data);
    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50, 0);
    central.writeUInt16LE(deflate ? 8 : 0, 10);
    central.writeUInt32LE(data.length, 20);
    central.writeUInt32LE(raw.length, 24);
    central.writeUInt16LE(nameBuf.length, 28);
    central.writeUInt32LE(offset, 42);
    centrals.push(central, nameBuf);
    offset += 30 + nameBuf.length + data.length;
  }
  const cd = Buffer.concat(centrals);
  const eocd = Buffer.alloc(22);
  eocd.writeUInt32LE(0x06054b50, 0);
  eocd.writeUInt16LE(Object.keys(files).length, 8);
  eocd.writeUInt16LE(Object.keys(files).length, 10);
  eocd.writeUInt32LE(cd.length, 12);
  eocd.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, cd, eocd]);
}

const param = (v, extra = {}) =>
  JSON.stringify({ titleId: "FAKE99261", contentVersion: v, applicationCategoryType: 0, ...extra });

test("the PS5 form of a version", () => {
  assert.equal(ps5ContentVersion("13.0.0"), "13.000.000");
  assert.equal(ps5ContentVersion("12.6.44"), "12.006.044");
  assert.equal(ps5ContentVersion("9.10.3"), "09.010.003");
  assert.equal(ps5ContentVersion("13.0.0-rc.1"), "13.000.000");
  assert.equal(ps5ContentVersion("100.0.0"), null);
  assert.equal(ps5ContentVersion("1.1000.0"), null);
  assert.equal(ps5ContentVersion("nope"), null);
});

test("param.json: version, title id and category", () => {
  assert.deepEqual(paramProblems(param("13.000.000"), "13.000.000", "p"), []);
  assert.match(paramProblems(param("12.006.044"), "13.000.000", "p")[0], /update-version/);
  assert.match(paramProblems(param("13.000.000", { titleId: "FAKE99260" }), "13.000.000", "p")[0], /titleId/);
  assert.match(
    paramProblems(param("13.000.000", { applicationCategoryType: 65536 }), "13.000.000", "p")[0],
    /applicationCategoryType/,
  );
  assert.match(paramProblems("{", "13.000.000", "p")[0], /not JSON/);
});

test("a good zip passes, stored or deflated", () => {
  for (const deflate of [false, true]) {
    const z = makeZip(
      {
        "FAKE99261/": "",
        "FAKE99261/eboot.bin": "ELF",
        "FAKE99261/sce_sys/param.json": param("13.000.000"),
      },
      deflate,
    );
    assert.deepEqual(zipProblems(z, "13.000.000", "z"), []);
    const e = zipEntries(z).get("FAKE99261/eboot.bin");
    assert.equal(zipRead(z, e).toString(), "ELF");
  }
});

test("a zip with the wrong version, no eboot, or another top folder fails", () => {
  const old = makeZip({ "FAKE99261/eboot.bin": "x", "FAKE99261/sce_sys/param.json": param("12.006.044") });
  assert.match(zipProblems(old, "13.000.000", "z").join("\n"), /contentVersion/);
  const noEboot = makeZip({ "FAKE99261/sce_sys/param.json": param("13.000.000") });
  assert.match(zipProblems(noEboot, "13.000.000", "z").join("\n"), /eboot\.bin/);
  const twoTops = makeZip({
    "FAKE99261/eboot.bin": "x",
    "FAKE99261/sce_sys/param.json": param("13.000.000"),
    "other/x": "y",
  });
  assert.match(zipProblems(twoTops, "13.000.000", "z").join("\n"), /top folder/);
  assert.match(zipProblems(Buffer.from("not a zip at all, nothing here"), "13.000.000", "z")[0], /not a zip/);
});
