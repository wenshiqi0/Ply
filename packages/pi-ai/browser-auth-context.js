// Embedded builds support host-supplied API keys only. Pi's default context
// includes lazy node:fs imports for ambient auth; none are needed here.
export function defaultProviderAuthContext() {
  return { env: async () => undefined, fileExists: async () => false };
}
