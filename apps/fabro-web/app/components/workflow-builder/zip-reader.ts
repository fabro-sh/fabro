/**
 * Reads a ZIP someone chose to import, trusting nothing in it.
 *
 * Sizes in a ZIP are claims. `fflate`'s `unzipSync` lets a filter see them, but a
 * stored entry then copies however many bytes its compressed size says, and many
 * entries can point at the same bytes, so a small file can unpack to far more than
 * any limit allows. So everything is checked here before a byte is unpacked:
 *
 * - The central directory and each local header have to agree, and entries can't
 *   overlap or repeat.
 * - Limits apply to the sizes, which then have to be true: a stored entry's sizes
 *   must match, a deflated one is stopped the moment it exceeds its claim, and every
 *   file's CRC is checked.
 *
 * An archive that fails any of this is refused whole. A well-formed file that is
 * merely too big, or compressed in a way we can't read, is left out and named.
 */

export interface ZipLimits {
  maxEntries: number;
  maxFileBytes: number;
  maxTotalBytes: number;
}

export interface ZipContents {
  files: Map<string, Uint8Array>;
  tooLarge: string[];
  unsupported: string[];
}

/** The part of `fflate`'s streaming inflater this uses. */
export type InflaterClass = new (
  ondata: (chunk: Uint8Array, final: boolean) => void,
) => { push(chunk: Uint8Array, final?: boolean): void };

/** Why an archive was refused, in words that finish "isn't a readable ZIP file:". */
export class ZipFormatError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ZipFormatError";
  }
}

const LOCAL_SIGNATURE = 0x04034b50;
const CENTRAL_SIGNATURE = 0x02014b50;
const END_SIGNATURE = 0x06054b50;
const END_SIZE = 22;
const LOCAL_SIZE = 30;
const CENTRAL_SIZE = 46;
const MAX_COMMENT = 0xffff;

const FLAG_ENCRYPTED = 1;
const FLAG_DATA_DESCRIPTOR = 1 << 3;
const STORED = 0;
const DEFLATED = 8;

/** Compressed bytes per step. At deflate's limit of about 1032:1, one step emits about 4 MiB, which bounds an overrun. */
const INFLATE_STEP = 4096;

interface Entry {
  name: string;
  nameBytes: Uint8Array;
  flags: number;
  method: number;
  crc: number;
  compressedSize: number;
  size: number;
  localOffset: number;
  /** Where its compressed bytes start, once the local header has been read. */
  dataStart: number;
}

function refuse(message: string): never {
  throw new ZipFormatError(message);
}

function findEndRecord(view: DataView): number {
  const last = view.byteLength - END_SIZE;
  for (let offset = last; offset >= 0 && offset >= last - MAX_COMMENT; offset--) {
    if (view.getUint32(offset, true) !== END_SIGNATURE) continue;
    // The comment has to end the file, or these bytes only look like the record.
    if (offset + END_SIZE + view.getUint16(offset + 20, true) === view.byteLength) return offset;
  }
  return refuse("it has no end-of-archive record");
}

function decodeName(bytes: Uint8Array): string {
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return new TextDecoder("latin1").decode(bytes);
  }
}

function readCentralDirectory(bytes: Uint8Array, view: DataView, maxEntries: number): { entries: Entry[]; start: number } {
  const end = findEndRecord(view);
  const disk = view.getUint16(end + 4, true);
  const directoryDisk = view.getUint16(end + 6, true);
  const onThisDisk = view.getUint16(end + 8, true);
  const count = view.getUint16(end + 10, true);
  const directorySize = view.getUint32(end + 12, true);
  const start = view.getUint32(end + 16, true);
  if (count === 0xffff || directorySize === 0xffffffff || start === 0xffffffff) refuse("it uses the ZIP64 format");
  if (disk !== 0 || directoryDisk !== 0 || onThisDisk !== count) refuse("it spans several disks");
  if (count > maxEntries) refuse(`it has ${count} entries, and the limit is ${maxEntries}`);
  if (start + directorySize !== end) refuse("its directory isn't where it says it is");

  const entries: Entry[] = [];
  const names = new Set<string>();
  let offset = start;
  for (let index = 0; index < count; index++) {
    if (offset + CENTRAL_SIZE > end || view.getUint32(offset, true) !== CENTRAL_SIGNATURE) {
      refuse("its directory is damaged");
    }
    const nameLength = view.getUint16(offset + 28, true);
    const extraLength = view.getUint16(offset + 30, true);
    const commentLength = view.getUint16(offset + 32, true);
    const next = offset + CENTRAL_SIZE + nameLength + extraLength + commentLength;
    if (next > end) refuse("its directory is damaged");
    const nameBytes = bytes.subarray(offset + CENTRAL_SIZE, offset + CENTRAL_SIZE + nameLength);
    const name = decodeName(nameBytes);
    if (names.has(name)) refuse(`it lists "${name}" twice`);
    names.add(name);
    entries.push({
      name,
      nameBytes,
      flags: view.getUint16(offset + 8, true),
      method: view.getUint16(offset + 10, true),
      crc: view.getUint32(offset + 16, true),
      compressedSize: view.getUint32(offset + 20, true),
      size: view.getUint32(offset + 24, true),
      localOffset: view.getUint32(offset + 42, true),
      dataStart: 0,
    });
    offset = next;
  }
  if (offset !== end) refuse("its directory has stray bytes");
  return { entries, start };
}

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((byte, index) => byte === b[index]);
}

/** Holds the local header to what the directory said, and rules out entries sharing bytes. */
function checkLocalHeaders(bytes: Uint8Array, view: DataView, entries: Entry[], directoryStart: number): void {
  const extents: { start: number; end: number; name: string }[] = [];
  for (const entry of entries) {
    const local = entry.localOffset;
    const fail = (what: string) => refuse(`the sizes or names recorded for "${entry.name}" disagree (${what})`);
    if (local + LOCAL_SIZE > directoryStart || view.getUint32(local, true) !== LOCAL_SIGNATURE) fail("no local header");
    const flags = view.getUint16(local + 6, true);
    const nameLength = view.getUint16(local + 26, true);
    const extraLength = view.getUint16(local + 28, true);
    const dataStart = local + LOCAL_SIZE + nameLength + extraLength;
    if (dataStart > directoryStart) fail("header runs past the data");
    if (!sameBytes(bytes.subarray(local + LOCAL_SIZE, local + LOCAL_SIZE + nameLength), entry.nameBytes)) fail("name");
    if (view.getUint16(local + 8, true) !== entry.method) fail("compression");
    if ((flags & FLAG_DATA_DESCRIPTOR) !== (entry.flags & FLAG_DATA_DESCRIPTOR)) fail("flags");
    const localCrc = view.getUint32(local + 14, true);
    const localCompressed = view.getUint32(local + 18, true);
    const localSize = view.getUint32(local + 22, true);
    if (entry.flags & FLAG_DATA_DESCRIPTOR) {
      // A streaming writer can't know these yet, and may leave them as zero.
      if (localCompressed !== 0 && localCompressed !== entry.compressedSize) fail("compressed size");
      if (localSize !== 0 && localSize !== entry.size) fail("size");
    } else {
      if (localCrc !== entry.crc) fail("checksum");
      if (localCompressed !== entry.compressedSize) fail("compressed size");
      if (localSize !== entry.size) fail("size");
    }
    const dataEnd = dataStart + entry.compressedSize;
    if (dataEnd > directoryStart) fail("data runs past the archive");
    entry.dataStart = dataStart;
    extents.push({ start: local, end: dataEnd, name: entry.name });
  }
  extents.sort((a, b) => a.start - b.start);
  for (let index = 1; index < extents.length; index++) {
    if (extents[index].start < extents[index - 1].end) {
      refuse(`"${extents[index - 1].name}" and "${extents[index].name}" share the same bytes`);
    }
  }
}

function makeCrcTable(): Uint32Array {
  const table = new Uint32Array(256);
  for (let index = 0; index < 256; index++) {
    let value = index;
    for (let bit = 0; bit < 8; bit++) value = value & 1 ? (value >>> 1) ^ 0xedb88320 : value >>> 1;
    table[index] = value >>> 0;
  }
  return table;
}

let crcTable: Uint32Array | null = null;

function crc32(bytes: Uint8Array): number {
  crcTable ??= makeCrcTable();
  let crc = 0xffffffff;
  for (const byte of bytes) crc = crcTable[(crc ^ byte) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}

/** Inflates to exactly `size` bytes, a step at a time, stopping as soon as the stream runs over. */
function inflateExactly(compressed: Uint8Array, size: number, Inflater: InflaterClass, name: string): Uint8Array {
  const output = new Uint8Array(size);
  let written = 0;
  const overrun = () => refuse(`"${name}" unpacks to more than its recorded size`);
  const inflater = new Inflater((chunk) => {
    if (written + chunk.length > size) overrun();
    output.set(chunk, written);
    written += chunk.length;
  });
  try {
    if (compressed.length === 0) inflater.push(compressed, true);
    for (let offset = 0; offset < compressed.length; offset += INFLATE_STEP) {
      inflater.push(compressed.subarray(offset, offset + INFLATE_STEP), offset + INFLATE_STEP >= compressed.length);
    }
  } catch (error) {
    if (error instanceof ZipFormatError) throw error;
    refuse(`"${name}" has damaged compressed data`);
  }
  if (written !== size) refuse(`"${name}" unpacks to less than its recorded size`);
  return output;
}

export function readZip(bytes: Uint8Array, Inflater: InflaterClass, limits: ZipLimits): ZipContents {
  if (bytes.length < END_SIZE) refuse("it is too short");
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const { entries, start } = readCentralDirectory(bytes, view, limits.maxEntries);
  checkLocalHeaders(bytes, view, entries, start);

  const contents: ZipContents = { files: new Map(), tooLarge: [], unsupported: [] };
  let total = 0;
  for (const entry of entries) {
    const { name, method, size, compressedSize } = entry;
    if (name.endsWith("/")) {
      if (size !== 0 || compressedSize !== 0) refuse(`the folder "${name}" holds data`);
      continue;
    }
    if (entry.flags & FLAG_ENCRYPTED) {
      contents.unsupported.push(name);
      continue;
    }
    if (method !== STORED && method !== DEFLATED) {
      contents.unsupported.push(name);
      continue;
    }
    if (method === STORED && compressedSize !== size) refuse(`the sizes recorded for "${name}" disagree`);
    if (size > limits.maxFileBytes || total + size > limits.maxTotalBytes) {
      contents.tooLarge.push(name);
      continue;
    }
    total += size;
    const data = bytes.subarray(entry.dataStart, entry.dataStart + compressedSize);
    const file = method === STORED ? data : inflateExactly(data, size, Inflater, name);
    if (crc32(file) !== entry.crc) refuse(`"${name}" fails its checksum`);
    contents.files.set(name, file);
  }
  return contents;
}
