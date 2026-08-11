let adapterPromise: Promise<typeof import("./mermaid")> | null = null;

/** Load the adapter once per WebView; retry a future diagram after a load failure. */
export function loadMermaidAdapter(): Promise<typeof import("./mermaid")> {
  if (!adapterPromise) {
    adapterPromise = import("./mermaid").catch((error) => {
      adapterPromise = null;
      throw error;
    });
  }
  return adapterPromise;
}
