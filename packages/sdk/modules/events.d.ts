declare module "ply:events" {
  export interface Event<T = unknown> {
    id: string;
    name: string;
    source: { plugin: string };
    data: T;
  }
  /** Define a local name; returns the native plugin-qualified name. */
  export function define(name: string): Promise<string>;
  /** During onLoad; waits until `<plugin>.<name>` is declared by its plugin. */
  export function on<T = unknown, R = unknown>(name: string, callback: (event: Event<T>) => R | Promise<R>): Promise<number>;
  export function off(subscription: number): Promise<boolean>;
  /** Only the owning plugin emits, after onLoad; listeners run serially, each as its own call. With no listener the event is only recorded. */
  export function emit<T = unknown, R = unknown>(name: string, data: T, options?: { timeout_ms?: number }): Promise<R[]>;
  export function list(): Promise<{ name: string; plugin: string; subscribers: number }[]>;
}


/** Native JSON HTTP transport; host bearer keys are never returned to JS. */
