import { describe, expect, test } from "bun:test";
import { deflateSync, Inflate, strToU8, zipSync } from "fflate";

import { importWorkflowFile } from "./bundle";
import { readZip, ZipFormatError, type InflaterClass, type ZipLimits } from "./zip-reader";

const MIB = 1024 * 1024;
const LIMITS: ZipLimits = { maxEntries: 1000, maxFileBytes: MIB, maxTotalBytes: 20 * MIB };

function crc32(bytes: Uint8Array): number {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++) crc = crc & 1 ? (crc >>> 1) ^ 0xedb88320 : crc >>> 1;
  }
  return (crc ^ 0xffffffff) >>> 0;
}

/** What gets written for one entry. Anything can be overridden, which is how an archive lies. */
interface ForgedEntry {
  name: string;
  data: Uint8Array;
  method?: number;
  /** Sizes and checksum as the central directory claims them. */
  size?: number;
  compressedSize?: number;
  crc?: number;
  flags?: number;
  /** Sizes, checksum and name as the local header claims them. */
  localSize?: number;
  localCompressedSize?: number;
  localCrc?: number;
  localName?: string;
  localMethod?: number;
  localFlags?: number;
  /** Write no local header, and point this entry's directory record at an earlier entry's. */
  sharesLocalOf?: number;
  /** Write no local header, and point this entry's directory record here. */
  localOffset?: number;
  /** Bytes between the compressed data and the next header, as a data descriptor would add. */
  trailer?: Uint8Array;
}

function concat(parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((sum, part) => sum + part.length, 0));
  let offset = 0;
  for (const part of parts) {
    out.set(part, offset);
    offset += part.length;
  }
  return out;
}

function record(fields: [number, 2 | 4][]): Uint8Array {
  const bytes = new Uint8Array(fields.reduce((sum, [, width]) => sum + width, 0));
  const view = new DataView(bytes.buffer);
  let offset = 0;
  for (const [value, width] of fields) {
    if (width === 2) view.setUint16(offset, value, true);
    else view.setUint32(offset, value, true);
    offset += width;
  }
  return bytes;
}

interface EndOverrides {
  count?: number;
  directoryOffset?: number;
  directorySize?: number;
  comment?: Uint8Array;
  disk?: number;
}

function localHeader(entry: ForgedEntry): Uint8Array {
  const name = strToU8(entry.localName ?? entry.name);
  return concat([
    record([
      [0x04034b50, 4],
      [20, 2],
      [entry.localFlags ?? entry.flags ?? 0, 2],
      [entry.localMethod ?? entry.method ?? 0, 2],
      [0, 2],
      [0, 2],
      [entry.localCrc ?? entry.crc ?? crc32(entry.data), 4],
      [entry.localCompressedSize ?? entry.compressedSize ?? entry.data.length, 4],
      [entry.localSize ?? entry.size ?? entry.data.length, 4],
      [name.length, 2],
      [0, 2],
    ]),
    name,
  ]);
}

/** A ZIP written by hand, so that every field can say something untrue. */
function forgeZip(entries: ForgedEntry[], end: EndOverrides = {}): Uint8Array {
  const sections: Uint8Array[] = [];
  const offsets: number[] = [];
  let position = 0;
  const push = (bytes: Uint8Array) => {
    sections.push(bytes);
    position += bytes.length;
  };
  for (const entry of entries) {
    if (entry.localOffset !== undefined || entry.sharesLocalOf !== undefined) {
      offsets.push(entry.localOffset ?? offsets[entry.sharesLocalOf as number]);
      continue;
    }
    offsets.push(position);
    push(localHeader(entry));
    push(entry.data);
    if (entry.trailer) push(entry.trailer);
  }
  const directoryStart = position;
  entries.forEach((entry, index) => {
    const name = strToU8(entry.name);
    push(
      record([
        [0x02014b50, 4],
        [20, 2],
        [20, 2],
        [entry.flags ?? 0, 2],
        [entry.method ?? 0, 2],
        [0, 2],
        [0, 2],
        [entry.crc ?? crc32(entry.data), 4],
        [entry.compressedSize ?? entry.data.length, 4],
        [entry.size ?? entry.data.length, 4],
        [name.length, 2],
        [0, 2],
        [0, 2],
        [0, 2],
        [0, 2],
        [0, 4],
        [offsets[index], 4],
      ]),
    );
    push(name);
  });
  const comment = end.comment ?? new Uint8Array();
  push(
    record([
      [0x06054b50, 4],
      [end.disk ?? 0, 2],
      [0, 2],
      [end.count ?? entries.length, 2],
      [end.count ?? entries.length, 2],
      [end.directorySize ?? position - directoryStart, 4],
      [end.directoryOffset ?? directoryStart, 4],
      [comment.length, 2],
    ]),
  );
  push(comment);
  return concat(sections);
}

function read(bytes: Uint8Array, limits: ZipLimits = LIMITS) {
  return readZip(bytes, Inflate as unknown as InflaterClass, limits);
}

function text(bytes: Uint8Array | undefined): string {
  return new TextDecoder().decode(bytes);
}

function refusedWith(bytes: Uint8Array, pattern: RegExp, limits: ZipLimits = LIMITS): void {
  let error: unknown;
  try {
    read(bytes, limits);
  } catch (caught) {
    error = caught;
  }
  expect(error).toBeInstanceOf(ZipFormatError);
  expect((error as Error).message).toMatch(pattern);
}

const stored = (name: string, content: string): ForgedEntry => ({ name, data: strToU8(content) });

describe("the review's repros", () => {
  test("16 entries over the same stored data can't turn 1.5 MiB into 24 MiB", () => {
    const shared = new Uint8Array(1536 * 1024).fill(97);
    const entries: ForgedEntry[] = Array.from({ length: 16 }, (_, index) => ({
      name: `prompts/${index}.md`,
      data: shared,
      size: 0,
      compressedSize: shared.length,
      crc: 0,
      ...(index === 0 ? {} : { sharesLocalOf: 0 }),
    }));
    expect(() => read(forgeZip(entries))).toThrow(ZipFormatError);
  });

  test("a second entry pointing at the first one's header can't borrow its data", () => {
    const shared = strToU8("the same bytes");
    refusedWith(
      forgeZip([
        { name: "a.md", data: shared },
        { name: "b.md", data: shared, sharesLocalOf: 0 },
      ]),
      /disagree/,
    );
  });

  test("a complete, honest entry hidden inside another's data is refused as an overlap", () => {
    const inner: ForgedEntry = { name: "inner.md", data: strToU8("inner") };
    const outer: ForgedEntry = { name: "outer.md", data: concat([localHeader(inner), inner.data]) };
    const zip = forgeZip([outer, { ...inner, localOffset: localHeader(outer).length }]);
    refusedWith(zip, /share the same bytes/);
  });

  test("a 2 MiB stored entry claiming to be empty can't slip past the per-file limit", () => {
    const data = new Uint8Array(2 * MIB).fill(97);
    refusedWith(forgeZip([{ name: "big.md", data, size: 0 }]), /disagree/);
  });

  test("the same entry claiming its true size is just too large, and named", () => {
    const data = new Uint8Array(2 * MIB).fill(97);
    const { files, tooLarge } = read(forgeZip([{ name: "big.md", data }]));
    expect(tooLarge).toEqual(["big.md"]);
    expect(files.size).toBe(0);
  });

  test("through the importer, the forged archive is refused with a reason", async () => {
    const shared = new Uint8Array(MIB).fill(97);
    const entries: ForgedEntry[] = Array.from({ length: 16 }, (_, index) => ({
      name: `prompts/${index}.md`,
      data: shared,
      size: 0,
      compressedSize: shared.length,
      crc: 0,
      ...(index === 0 ? {} : { sharesLocalOf: 0 }),
    }));
    entries.push(stored("workflow.fabro", "digraph A {\n}\n"));
    const file = new File([forgeZip(entries)], "forged.zip");
    await expect(importWorkflowFile(file)).rejects.toThrow("not a readable ZIP");
  });
});

describe("deflated data", () => {
  const bomb = () => deflateSync(new Uint8Array(64 * MIB), { level: 6 });

  /** Counts what the inflater actually produced, which is the real budget. */
  function countingInflater() {
    const counter = { emitted: 0 };
    class Counting extends (Inflate as unknown as InflaterClass) {
      constructor(ondata: (chunk: Uint8Array, final: boolean) => void) {
        super((chunk, final) => {
          counter.emitted += chunk.length;
          ondata(chunk, final);
        });
      }
    }
    return { counter, Inflater: Counting as unknown as InflaterClass };
  }

  test("a bomb claiming a tiny size is stopped almost at once, not unpacked", () => {
    const { counter, Inflater } = countingInflater();
    const zip = forgeZip([{ name: "bomb.md", data: bomb(), method: 8, size: 100, crc: 0 }]);
    expect(() => readZip(zip, Inflater, LIMITS)).toThrow(ZipFormatError);
    // One step of compressed input at deflate's maximum ratio, far below the 64 MiB inside.
    expect(counter.emitted).toBeLessThan(5 * MIB);
  });

  test("a bomb claiming its true size is left out without being unpacked at all", () => {
    const { counter, Inflater } = countingInflater();
    const zip = forgeZip([{ name: "bomb.md", data: bomb(), method: 8, size: 64 * MIB, crc: 0 }]);
    const { files, tooLarge } = readZip(zip, Inflater, LIMITS);
    expect(tooLarge).toEqual(["bomb.md"]);
    expect(files.size).toBe(0);
    expect(counter.emitted).toBe(0);
  });

  test("a stream shorter than it claims is refused", () => {
    const data = deflateSync(strToU8("short"));
    refusedWith(forgeZip([{ name: "a.md", data, method: 8, size: 500, crc: crc32(strToU8("short")) }]), /less than/);
  });

  test("a stream longer than it claims is refused", () => {
    const data = deflateSync(strToU8("much longer than that"));
    refusedWith(forgeZip([{ name: "a.md", data, method: 8, size: 4, crc: 0 }]), /more than/);
  });

  test("garbage in place of a stream is refused", () => {
    refusedWith(forgeZip([{ name: "a.md", data: new Uint8Array([255, 255, 255, 255]), method: 8, size: 4 }]), /damaged/);
  });

  test("the right size with the wrong content fails the checksum", () => {
    const data = deflateSync(strToU8("hello"));
    refusedWith(forgeZip([{ name: "a.md", data, method: 8, size: 5, crc: 12345 }]), /checksum/);
  });
});

describe("metadata has to agree", () => {
  const data = strToU8("hello");

  test.each<[string, Partial<ForgedEntry>, RegExp]>([
    ["local size", { localSize: 99 }, /size/],
    ["local compressed size", { localCompressedSize: 99 }, /compressed size/],
    ["local checksum", { localCrc: 99 }, /checksum/],
    ["local name", { localName: "other.md" }, /name/],
    ["local compression", { localMethod: 8 }, /compression/],
    ["local flags", { localFlags: 8 }, /flags/],
  ])("a differing %s is refused", (_field, overrides, pattern) => {
    refusedWith(forgeZip([{ name: "a.md", data, ...overrides }]), pattern);
  });

  test("a stored entry's sizes have to match each other", () => {
    refusedWith(forgeZip([{ name: "a.md", data, size: 3 }]), /disagree/);
  });

  test("a wrong checksum on stored data is refused", () => {
    refusedWith(forgeZip([{ name: "a.md", data, crc: 1 }]), /checksum/);
  });

  test("the same name twice is refused", () => {
    refusedWith(forgeZip([stored("a.md", "one"), stored("a.md", "two")]), /twice/);
  });

  test("a compressed size running past the archive is refused", () => {
    refusedWith(forgeZip([{ name: "a.md", data, compressedSize: 9 * MIB, localCompressedSize: 9 * MIB, size: 9 * MIB, localSize: 9 * MIB }]), /past/);
  });

  test("an offset pointing outside the data is refused", () => {
    refusedWith(forgeZip([{ ...stored("a.md", "one"), localOffset: 0x7fffffff }]), /no local header/);
  });

  test("a folder entry can't carry data", () => {
    refusedWith(forgeZip([{ name: "folder/", data }]), /folder/);
  });

  test("the directory has to be where the end record says", () => {
    refusedWith(forgeZip([stored("a.md", "one")], { directoryOffset: 1 }), /directory/);
    refusedWith(forgeZip([stored("a.md", "one")], { directorySize: 1 }), /directory/);
  });

  test("an end record listing more entries than exist is refused", () => {
    refusedWith(forgeZip([stored("a.md", "one")], { count: 5 }), /directory/);
  });

  test("an end record listing fewer entries than exist is refused", () => {
    refusedWith(forgeZip([stored("a.md", "one"), stored("b.md", "two")], { count: 1 }), /stray/);
  });

  test("ZIP64 and multi-disk archives are refused plainly", () => {
    refusedWith(forgeZip([stored("a.md", "one")], { count: 0xffff }), /ZIP64/);
    refusedWith(forgeZip([stored("a.md", "one")], { disk: 1 }), /disks/);
  });

  test("something that isn't a ZIP is refused", () => {
    refusedWith(strToU8("definitely not a zip file at all"), /end-of-archive/);
    refusedWith(new Uint8Array(3), /too short/);
  });
});

describe("limits", () => {
  test("too many entries are refused before any is read", () => {
    const entries = Array.from({ length: 11 }, (_, index) => stored(`${index}.md`, "x"));
    refusedWith(forgeZip(entries), /11 entries/, { ...LIMITS, maxEntries: 10 });
  });

  test("honest files past the total are left out and named, and the real total is what's kept", () => {
    const fileSize = 1024;
    const entries = Array.from({ length: 5 }, (_, index) => ({ name: `${index}.md`, data: new Uint8Array(fileSize).fill(97) }));
    const { files, tooLarge } = read(forgeZip(entries), { ...LIMITS, maxTotalBytes: 3 * fileSize });
    expect([...files.keys()]).toEqual(["0.md", "1.md", "2.md"]);
    expect(tooLarge).toEqual(["3.md", "4.md"]);
    expect([...files.values()].reduce((sum, file) => sum + file.length, 0)).toBe(3 * fileSize);
  });

  test("a file exactly at the limit is kept", () => {
    const { files, tooLarge } = read(forgeZip([{ name: "a.md", data: new Uint8Array(MIB).fill(97) }]));
    expect(files.get("a.md")?.length).toBe(MIB);
    expect(tooLarge).toEqual([]);
  });

  test("compression it can't read, or encryption, is named rather than refused", () => {
    const { files, unsupported } = read(
      forgeZip([
        { name: "a.md", data: strToU8("x"), method: 12 },
        { name: "b.md", data: strToU8("x"), flags: 1 },
        stored("c.md", "kept"),
      ]),
    );
    expect(unsupported).toEqual(["a.md", "b.md"]);
    expect(text(files.get("c.md"))).toBe("kept");
  });
});

describe("real archives", () => {
  test("what the builder itself writes reads back, at any compression level", () => {
    const entries = { "workflow.fabro": strToU8("digraph A {\n}\n"), "shared/ticket.md": strToU8("# Ticket\n".repeat(500)) };
    for (const level of [0, 1, 6, 9] as const) {
      const { files } = read(zipSync(entries, { level }));
      expect(text(files.get("workflow.fabro"))).toBe("digraph A {\n}\n");
      expect(text(files.get("shared/ticket.md"))).toBe("# Ticket\n".repeat(500));
    }
  });

  test("a streaming writer's data descriptor, with zero sizes up front, is fine", () => {
    const content = strToU8("written by a stream");
    const data = deflateSync(content);
    const crc = crc32(content);
    const descriptor = record([
      [0x08074b50, 4],
      [crc, 4],
      [data.length, 4],
      [content.length, 4],
    ]);
    const { files } = read(
      forgeZip([
        {
          name: "a.md",
          data,
          method: 8,
          flags: 8,
          size: content.length,
          crc,
          localSize: 0,
          localCompressedSize: 0,
          localCrc: 0,
          trailer: descriptor,
        },
        stored("b.md", "after it"),
      ]),
    );
    expect(text(files.get("a.md"))).toBe("written by a stream");
    expect(text(files.get("b.md"))).toBe("after it");
  });

  test("an archive comment is fine, even one containing a false end record", () => {
    const fake = record([[0x06054b50, 4]]);
    const { files } = read(forgeZip([stored("a.md", "kept")], { comment: concat([strToU8("note "), fake, strToU8(" end")]) }));
    expect(text(files.get("a.md"))).toBe("kept");
  });

  test("folder entries and empty files are fine", () => {
    const { files } = read(forgeZip([{ name: "folder/", data: new Uint8Array() }, stored("folder/empty.md", "")]));
    expect([...files.keys()]).toEqual(["folder/empty.md"]);
    expect(files.get("folder/empty.md")?.length).toBe(0);
  });

  test("an empty deflated file is fine", () => {
    const { files } = read(forgeZip([{ name: "a.md", data: deflateSync(new Uint8Array()), method: 8, size: 0, crc: 0 }]));
    expect(files.get("a.md")?.length).toBe(0);
  });

  test("an empty archive holds nothing", () => {
    expect(read(forgeZip([])).files.size).toBe(0);
  });
});
