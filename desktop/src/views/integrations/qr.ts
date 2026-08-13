/* ============================================================================
   A minimal QR encoder — byte mode, error-correction level L, versions 1-15.

   WhatsApp pairing hands us a reference string that only exists as a picture on
   the user's phone screen, so the code has to be drawn locally. That is the
   whole reason this file exists rather than an npm dependency: the app ships
   offline and one small encoder is cheaper than a package.

   Scope is deliberate. Byte mode encodes any UTF-8 payload, level L maximises
   capacity (a screen-rendered code is not going to be scuffed), and version 15
   carries 520 bytes — several times the length of a pairing reference.

   Verified against the ISO/IEC 18004 Reed-Solomon worked example, the published
   format- and version-information tables, and a round-trip decode that reads
   each generated matrix back the way a scanner does.
   ========================================================================== */

/* --- GF(256) ------------------------------------------------------------- */

const EXP = new Uint8Array(512);
const LOG = new Uint8Array(256);
{
  let x = 1;
  for (let i = 0; i < 255; i++) {
    EXP[i] = x;
    LOG[x] = i;
    x <<= 1;
    // The QR field polynomial, x^8 + x^4 + x^3 + x^2 + 1.
    if (x & 0x100) x ^= 0x11d;
  }
  for (let i = 255; i < 512; i++) EXP[i] = EXP[i - 255];
}

function gmul(a: number, b: number): number {
  if (a === 0 || b === 0) return 0;
  return EXP[LOG[a] + LOG[b]];
}

/** g(x) = (x - a^0)(x - a^1)…, highest-degree coefficient first. */
function generatorPoly(n: number): number[] {
  let poly = [1];
  for (let i = 0; i < n; i++) {
    const next = new Array<number>(poly.length + 1).fill(0);
    for (let j = 0; j < poly.length; j++) {
      next[j] ^= poly[j];
      next[j + 1] ^= gmul(poly[j], EXP[i]);
    }
    poly = next;
  }
  return poly;
}

function rsEncode(data: number[], ecLen: number): number[] {
  const gen = generatorPoly(ecLen);
  const res = new Array<number>(data.length + ecLen).fill(0);
  for (let i = 0; i < data.length; i++) res[i] = data[i];
  for (let i = 0; i < data.length; i++) {
    const factor = res[i];
    if (factor === 0) continue;
    for (let j = 0; j < gen.length; j++) res[i + j] ^= gmul(gen[j], factor);
  }
  return res.slice(data.length);
}

/* --- Version tables (level L) -------------------------------------------- */

/** [ecCodewordsPerBlock, group1Blocks, group1Data, group2Blocks, group2Data] */
type EccSpec = [number, number, number, number, number];

const ECC_L: (EccSpec | null)[] = [
  null,
  [7, 1, 19, 0, 0],
  [10, 1, 34, 0, 0],
  [15, 1, 55, 0, 0],
  [20, 1, 80, 0, 0],
  [26, 1, 108, 0, 0],
  [18, 2, 68, 0, 0],
  [20, 2, 78, 0, 0],
  [24, 2, 97, 0, 0],
  [30, 2, 116, 0, 0],
  [18, 2, 68, 2, 69],
  [20, 4, 81, 0, 0],
  [24, 2, 92, 2, 93],
  [26, 4, 107, 0, 0],
  [30, 3, 115, 1, 116],
  [22, 5, 87, 1, 88],
];

const ALIGN: number[][] = [
  [], [], [6, 18], [6, 22], [6, 26], [6, 30], [6, 34], [6, 22, 38],
  [6, 24, 42], [6, 26, 46], [6, 28, 50], [6, 30, 54], [6, 32, 58],
  [6, 34, 62], [6, 26, 46, 66], [6, 26, 48, 70],
];

const MAX_VERSION = ECC_L.length - 1;

function specFor(v: number): EccSpec {
  const spec = ECC_L[v];
  if (!spec) throw new Error(`unsupported QR version ${v}`);
  return spec;
}

function dataCapacity(v: number): number {
  const [, b1, d1, b2, d2] = specFor(v);
  return b1 * d1 + b2 * d2;
}

/** Smallest version that fits, or 0 when the payload is too long. */
function pickVersion(byteLen: number): number {
  for (let v = 1; v <= MAX_VERSION; v++) {
    const countBits = v < 10 ? 8 : 16;
    if (4 + countBits + byteLen * 8 <= dataCapacity(v) * 8) return v;
  }
  return 0;
}

/* --- Data codewords ------------------------------------------------------ */

function buildCodewords(bytes: number[], v: number): number[] {
  const total = dataCapacity(v);
  const bits: number[] = [];
  const push = (val: number, len: number) => {
    for (let i = len - 1; i >= 0; i--) bits.push((val >> i) & 1);
  };

  push(0b0100, 4); // byte mode
  push(bytes.length, v < 10 ? 8 : 16);
  for (const b of bytes) push(b, 8);

  // Terminator, then pad out to a whole byte.
  for (let i = 0; i < 4 && bits.length < total * 8; i++) bits.push(0);
  while (bits.length % 8 !== 0) bits.push(0);

  const data: number[] = [];
  for (let i = 0; i < bits.length; i += 8) {
    let byte = 0;
    for (let j = 0; j < 8; j++) byte = (byte << 1) | bits[i + j];
    data.push(byte);
  }

  // The specified alternating filler for the unused remainder.
  const PAD = [0xec, 0x11];
  for (let i = 0; data.length < total; i++) data.push(PAD[i % 2]);
  return data;
}

/** Split into blocks, add parity, then interleave as the spec requires. */
function interleave(data: number[], v: number): number[] {
  const [ecLen, b1, d1, b2, d2] = specFor(v);
  const blocks: number[][] = [];
  let off = 0;
  for (let i = 0; i < b1; i++) {
    blocks.push(data.slice(off, off + d1));
    off += d1;
  }
  for (let i = 0; i < b2; i++) {
    blocks.push(data.slice(off, off + d2));
    off += d2;
  }
  const ecBlocks = blocks.map((b) => rsEncode(b, ecLen));

  const out: number[] = [];
  for (let i = 0; i < Math.max(d1, d2); i++) {
    for (const b of blocks) if (i < b.length) out.push(b[i]);
  }
  for (let i = 0; i < ecLen; i++) {
    for (const b of ecBlocks) out.push(b[i]);
  }
  return out;
}

/* --- Format and version information -------------------------------------- */

const FORMAT_MASK = 0b101010000010010;

function formatBits(mask: number): number {
  const data = (0b01 << 3) | mask; // 01 = error-correction level L
  let rem = data << 10;
  for (let i = 14; i >= 10; i--) {
    if ((rem >> i) & 1) rem ^= 0b10100110111 << (i - 10);
  }
  return ((data << 10) | rem) ^ FORMAT_MASK;
}

function versionBits(v: number): number {
  let rem = v << 12;
  for (let i = 17; i >= 12; i--) {
    if ((rem >> i) & 1) rem ^= 0b1111100100101 << (i - 12);
  }
  return (v << 12) | rem;
}

/* --- Matrix -------------------------------------------------------------- */

type MaskFn = (r: number, c: number) => boolean;

const MASKS: MaskFn[] = [
  (r, c) => (r + c) % 2 === 0,
  (r) => r % 2 === 0,
  (_r, c) => c % 3 === 0,
  (r, c) => (r + c) % 3 === 0,
  (r, c) => (Math.floor(r / 2) + Math.floor(c / 3)) % 2 === 0,
  (r, c) => ((r * c) % 2) + ((r * c) % 3) === 0,
  (r, c) => ((((r * c) % 2) + ((r * c) % 3)) % 2) === 0,
  (r, c) => ((((r + c) % 2) + ((r * c) % 3)) % 2) === 0,
];

export type QRMatrix = number[][];

function buildMatrix(codewords: number[], v: number, mask: number): QRMatrix {
  const size = v * 4 + 17;
  const m: QRMatrix = Array.from({ length: size }, () => new Array<number>(size).fill(0));
  const fixed = Array.from({ length: size }, () => new Array<boolean>(size).fill(false));

  const set = (r: number, c: number, val: number) => {
    if (r < 0 || r >= size || c < 0 || c >= size) return;
    m[r][c] = val;
    fixed[r][c] = true;
  };

  // Finder patterns plus their separators.
  const finder = (r0: number, c0: number) => {
    for (let r = -1; r <= 7; r++) {
      for (let c = -1; c <= 7; c++) {
        const inRing =
          (r >= 0 && r <= 6 && (c === 0 || c === 6)) ||
          (c >= 0 && c <= 6 && (r === 0 || r === 6));
        const inCore = r >= 2 && r <= 4 && c >= 2 && c <= 4;
        set(r0 + r, c0 + c, inRing || inCore ? 1 : 0);
      }
    }
  };
  finder(0, 0);
  finder(0, size - 7);
  finder(size - 7, 0);

  // Timing patterns.
  for (let i = 8; i < size - 8; i++) {
    set(6, i, i % 2 === 0 ? 1 : 0);
    set(i, 6, i % 2 === 0 ? 1 : 0);
  }

  // Alignment patterns, minus the three that would sit on a finder.
  const centers = ALIGN[v];
  for (const r0 of centers) {
    for (const c0 of centers) {
      if (
        (r0 <= 8 && c0 <= 8) ||
        (r0 <= 8 && c0 >= size - 9) ||
        (r0 >= size - 9 && c0 <= 8)
      ) {
        continue;
      }
      for (let r = -2; r <= 2; r++) {
        for (let c = -2; c <= 2; c++) {
          set(r0 + r, c0 + c, Math.max(Math.abs(r), Math.abs(c)) === 1 ? 0 : 1);
        }
      }
    }
  }

  // Both format copies, reserved before any data is laid down.
  const fmt = formatBits(mask);
  for (let i = 0; i < 15; i++) {
    const bit = (fmt >> i) & 1;
    if (i < 6) set(8, i, bit);
    else if (i === 6) set(8, 7, bit);
    else if (i === 7) set(8, 8, bit);
    else if (i === 8) set(7, 8, bit);
    else set(14 - i, 8, bit);
    // Bits 0-6 climb the bottom-left, 7-14 run across the top-right.
    if (i < 7) set(size - 1 - i, 8, bit);
    else set(8, size - 15 + i, bit);
  }

  // The module that is dark in every symbol.
  set(size - 8, 8, 1);

  if (v >= 7) {
    const vb = versionBits(v);
    for (let i = 0; i < 18; i++) {
      const bit = (vb >> i) & 1;
      const r = Math.floor(i / 3);
      const c = size - 11 + (i % 3);
      set(r, c, bit);
      set(c, r, bit);
    }
  }

  // Data, zigzagging up and down column pairs from the right, skipping the
  // vertical timing column. Masking is applied as each module is written.
  const totalBits = codewords.length * 8;
  let bitIdx = 0;
  let up = true;
  for (let col = size - 1; col > 0; col -= 2) {
    if (col === 6) col--;
    for (let n = 0; n < size; n++) {
      const row = up ? size - 1 - n : n;
      for (const c of [col, col - 1]) {
        if (fixed[row][c]) continue;
        let bit = 0;
        if (bitIdx < totalBits) {
          bit = (codewords[bitIdx >> 3] >> (7 - (bitIdx & 7))) & 1;
          bitIdx++;
        }
        m[row][c] = MASKS[mask](row, c) ? bit ^ 1 : bit;
      }
    }
    up = !up;
  }

  return m;
}

/* --- Mask selection ------------------------------------------------------ */

function penalty(m: QRMatrix): number {
  const size = m.length;
  let score = 0;

  // Rule 1 — runs of five or more same-coloured modules.
  const runScore = (run: number) => (run >= 5 ? run - 2 : 0);
  for (let i = 0; i < size; i++) {
    let rRun = 1;
    let cRun = 1;
    for (let j = 1; j < size; j++) {
      if (m[i][j] === m[i][j - 1]) rRun++;
      else {
        score += runScore(rRun);
        rRun = 1;
      }
      if (m[j][i] === m[j - 1][i]) cRun++;
      else {
        score += runScore(cRun);
        cRun = 1;
      }
    }
    score += runScore(rRun) + runScore(cRun);
  }

  // Rule 2 — 2x2 blocks of one colour.
  for (let r = 0; r < size - 1; r++) {
    for (let c = 0; c < size - 1; c++) {
      const v = m[r][c];
      if (v === m[r][c + 1] && v === m[r + 1][c] && v === m[r + 1][c + 1]) score += 3;
    }
  }

  // Rule 3 — patterns that imitate a finder.
  const P1 = [1, 0, 1, 1, 1, 0, 1, 0, 0, 0, 0];
  const P2 = [0, 0, 0, 0, 1, 0, 1, 1, 1, 0, 1];
  const match = (get: (k: number) => number, i: number) =>
    P1.every((b, k) => get(i + k) === b) || P2.every((b, k) => get(i + k) === b);
  for (let i = 0; i < size; i++) {
    for (let j = 0; j + 11 <= size; j++) {
      if (match((k) => m[i][k], j)) score += 40;
      if (match((k) => m[k][i], j)) score += 40;
    }
  }

  // Rule 4 — deviation from an even balance of dark and light.
  let dark = 0;
  for (const row of m) for (const v of row) dark += v;
  const pct = (dark * 100) / (size * size);
  score += Math.floor(Math.abs(pct - 50) / 5) * 10;

  return score;
}

/* --- Public API ---------------------------------------------------------- */

/**
 * Encode `text` as a QR matrix of 0/1 modules, or null when it is longer than
 * version 15 at level L can carry (520 bytes).
 */
export function encodeQR(text: string): QRMatrix | null {
  const bytes = Array.from(new TextEncoder().encode(text));
  const v = pickVersion(bytes.length);
  if (v === 0) return null;

  const codewords = interleave(buildCodewords(bytes, v), v);

  let best: QRMatrix | null = null;
  let bestScore = Infinity;
  for (let mask = 0; mask < 8; mask++) {
    const candidate = buildMatrix(codewords, v, mask);
    const score = penalty(candidate);
    if (score < bestScore) {
      bestScore = score;
      best = candidate;
    }
  }
  return best;
}

/**
 * An SVG path covering every dark module, built from horizontal runs so the
 * markup stays small on the larger versions.
 */
export function qrPath(m: QRMatrix): string {
  const parts: string[] = [];
  for (let r = 0; r < m.length; r++) {
    let c = 0;
    while (c < m.length) {
      if (m[r][c] !== 1) {
        c++;
        continue;
      }
      let end = c;
      while (end + 1 < m.length && m[r][end + 1] === 1) end++;
      parts.push(`M${c} ${r}h${end - c + 1}v1h-${end - c + 1}z`);
      c = end + 1;
    }
  }
  return parts.join("");
}
