import type { ModelCatalog, PickerCatalog } from "./model-picker-model";

/**
 * Test data, imported only by tests. The names are made up: the picker reads whatever the
 * server sends and has no list of its own.
 */
export const TEST_CATALOG: ModelCatalog = {
  providers: [
    { id: "acme", display_name: "Acme AI", configured: true, priority: 90, aliases: ["acme-ai"], default_model: "acme-large" },
    { id: "router", display_name: "RouterCo", configured: false, priority: 50, aliases: ["rc"], default_model: "router-fast" },
    { id: "local", display_name: "Local Gateway", configured: false, priority: 5, aliases: [], default_model: null },
  ],
  models: [
    { id: "acme-large", provider: "acme", family: "atlas", display_name: "Acme Large", aliases: ["large"], default: true, small_default: false, configured: true },
    { id: "acme-small", provider: "acme", family: "atlas", display_name: "Acme Small", aliases: ["small"], default: false, small_default: true, configured: true },
    { id: "acme-large", provider: "router", family: "atlas", display_name: "Acme Large via Router", aliases: [], default: false, small_default: false, configured: false },
    { id: "router-fast", provider: "router", family: "bolt", display_name: "Bolt 5.5", aliases: ["bolt-latest"], default: true, small_default: false, configured: false },
    { id: "orphan-1", provider: "unlisted", family: "misc", display_name: "Orphan One", aliases: [], default: false, small_default: false, configured: false },
  ],
};

export function testPickerCatalog(overrides: Partial<PickerCatalog> = {}): PickerCatalog {
  return {
    ...TEST_CATALOG,
    status: "ready",
    providersFailed: false,
    retrying: false,
    retry: () => {},
    ...overrides,
  };
}
