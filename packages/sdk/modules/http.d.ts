declare module "ply:http" {
  export function hasCredentials(provider: string): boolean;
  export function requestJson(request: {
    url: string;
    body: unknown;
    auth_provider?: string;
    timeout_ms?: number;
  }): Promise<{ status: number; data: unknown }>;
}
