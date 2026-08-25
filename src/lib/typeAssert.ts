/**
 * Compile-time assertions — the repo's only regression guard for a *value*.
 *
 * There is no TypeScript test harness here: `npm run build` is
 * `tsc --noEmit && vite build`, and that is the whole frontend gate. So a value
 * that must not drift — a wire sentinel, a base URL a third-party SDK appends
 * to — is pinned by giving it a literal type and asserting that type exactly.
 * Respelling the value then fails `tsc`, i.e. fails CI.
 *
 * The shape, at every call site:
 *
 * ```ts
 * export const MARKER = "\u0001";
 * export type PinMarker = Expect<Eq<typeof MARKER, "\u0001">>;
 * ```
 *
 * Two rules the idiom depends on, both easy to lose:
 *
 * * **`Eq`, not `extends`.** `"a" extends string` is true, so an `extends` check
 *   passes against a value widened to `string` and pins nothing.
 * * **Export the alias.** `noUnusedLocals` deletes an unused local type, and a
 *   guard the compiler is allowed to ignore is decoration.
 *
 * Introduced by #427 for the gateway base URLs and lifted here by #438, whose
 * snippet sentinels needed the same thing — a second byte-identical copy is how
 * an idiom quietly becomes two idioms that disagree.
 */

/** Exact type equality — `extends` alone would accept a wider literal. */
export type Eq<A, B> =
  (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2
    ? true
    : false;

/** Fails to compile unless `T` is exactly `true`. */
export type Expect<T extends true> = T;
