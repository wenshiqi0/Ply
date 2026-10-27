declare module "ply:checkpoints" {
  export function create(snapshot: unknown): Promise<string>;
  export function claim(id: string): Promise<{ lease: number; snapshot: unknown }>;
  export function update(lease: number, snapshot: unknown): Promise<void>;
  export function finish(lease: number, remove: boolean): Promise<void>;
}

/**
 * Runtime-owned cleanup after failure/interruption. The whole batch has a
 * two-second deadline; its pending work is cancelled when the batch ends.
 * Await cleanup work that must finish. Ordinary success uses your finally.
 */
