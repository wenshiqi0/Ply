// What the prelude gives the runtime's own Node modules (RFC 0015) as
// "ply:runtime". Plugin code cannot resolve this name. The realm's globals
// (Buffer, process, …) are the ones @types/node describes.
declare module "ply:runtime" {
  /** Gives a Uint8Array Buffer's prototype, without copying. */
  export function toBuffer(bytes: Uint8Array): Buffer;
  export function encode(text: string, encoding?: string): Uint8Array;
  export function decode(bytes: Uint8Array, encoding?: string): string;
  export function inspect(value: unknown, options?: { depth?: number }): string;
  export function format(...args: unknown[]): string;
  export function deepEqual(a: unknown, b: unknown, memo: Map<unknown, unknown>): boolean;
}
