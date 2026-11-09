import type { Model, Provider } from "@qltysh/fabro-api-client";

import { modelOfferingKey } from "../../lib/model-offerings";
import {
  effectiveNodeAttributes,
  getAttr,
  groupChain,
  groupsById,
  nodeClasses,
} from "../../lib/workflow-builder/effective";
import {
  hasTemplateSyntax,
  parseStylesheet,
  selectorSpecificity,
} from "../../lib/workflow-builder/expressions";
import { shapeOf } from "../../lib/workflow-builder/kinds";
import type { WorkflowGraph, WorkflowNode } from "../../lib/workflow-builder/types";
import type { EditorChange, EditorContext } from "./editor-state";
import { updateGraphAttributes, updateNodeAttributes } from "./graph-ops";

/**
 * What the model picker knows, with no React in it. The catalog comes from the
 * server and nothing here invents a provider or a model. A model ID is unique
 * only within its provider, so every identity below is the pair.
 */

export type CatalogModel = Pick<
  Model,
  "id" | "provider" | "family" | "display_name" | "aliases" | "default" | "small_default" | "configured"
>;

export type CatalogProvider = Pick<
  Provider,
  "id" | "display_name" | "configured" | "priority" | "aliases" | "default_model"
>;

export interface ModelCatalog {
  models: readonly CatalogModel[];
  providers: readonly CatalogProvider[];
}

/** The attributes the picker owns, as written. A missing key inherits. */
export interface ModelChoice {
  model?: string;
  provider?: string;
}

export interface InheritedValue {
  value: string;
  /** Where it comes from, in the author's words. */
  source: string;
}

/** What applies when this node sets nothing. Model and provider fall back independently, as in Petri. */
export interface InheritedModel {
  model?: InheritedValue;
  provider?: InheritedValue;
  /** The stylesheet is a file or a template, so only the run can say what it sets. */
  stylesheetOpaque?: boolean;
}

export const NO_INHERITED_MODEL: InheritedModel = {};

// ── The catalog as provider tiers ───────────────────────────────────────────

export interface ProviderGroup {
  id: string;
  name: string;
  /** False when nothing here can run until the provider is set up in Settings. */
  ready: boolean;
  models: CatalogModel[];
}

/**
 * One group per provider. A provider with no models still appears, so it can be
 * found by name and reached with a custom ID. A model whose provider the provider
 * list doesn't name still appears, under its provider ID.
 */
export function buildProviderGroups(catalog: ModelCatalog): ProviderGroup[] {
  const byProvider = new Map<string, CatalogModel[]>();
  for (const model of catalog.models) {
    const list = byProvider.get(model.provider);
    if (list) list.push(model);
    else byProvider.set(model.provider, [model]);
  }
  const known = new Map(catalog.providers.map((provider) => [provider.id, provider]));
  const ids = new Set([...catalog.providers.map((provider) => provider.id), ...byProvider.keys()]);
  const rank = (id: string) => known.get(id)?.priority ?? Number.NEGATIVE_INFINITY;

  return [...ids]
    .map((id): ProviderGroup => {
      const models = byProvider.get(id) ?? [];
      const provider = known.get(id);
      return {
        id,
        name: provider?.display_name || id,
        ready: models.length > 0 ? models.some((model) => model.configured) : (provider?.configured ?? false),
        models,
      };
    })
    .sort(
      (a, b) =>
        Number(b.models.length > 0) - Number(a.models.length > 0) ||
        Number(b.ready) - Number(a.ready) ||
        rank(b.id) - rank(a.id) ||
        a.name.localeCompare(b.name),
    );
}

// ── Search ──────────────────────────────────────────────────────────────────

interface SearchToken {
  raw: string;
  /** Without punctuation, so `gpt5` finds `gpt-5` and `opus 5.5` finds `claude-opus-5-5`. */
  compact: string;
}

function fold(text: string): string {
  return text.normalize("NFKD").toLowerCase();
}

function compactOf(foldedText: string): string {
  return foldedText.replace(/[^\p{L}\p{N}]+/gu, "");
}

export function searchTokens(query: string): SearchToken[] {
  return fold(query)
    .split(/\s+/)
    .filter(Boolean)
    .map((raw) => ({ raw, compact: compactOf(raw) }));
}

function fieldMatches(field: string, token: SearchToken): boolean {
  const folded = fold(field);
  return folded.includes(token.raw) || (token.compact !== "" && compactOf(folded).includes(token.compact));
}

function everyToken(tokens: SearchToken[], fields: readonly string[]): boolean {
  return tokens.every((token) => fields.some((field) => fieldMatches(field, token)));
}

function providerFields(id: string, name: string, provider: CatalogProvider | undefined): string[] {
  return [id, name, ...(provider?.aliases ?? [])];
}

function modelFields(model: CatalogModel): string[] {
  return [model.id, model.display_name, model.family, ...model.aliases];
}

export interface GroupMatch {
  group: ProviderGroup;
  models: CatalogModel[];
}

/**
 * Every word must appear somewhere in the model or its provider, so `anthropic opus`
 * narrows and a company name alone lists that company's models. An empty query
 * matches everything.
 */
export function searchGroups(
  groups: readonly ProviderGroup[],
  providers: readonly CatalogProvider[],
  query: string,
): GroupMatch[] {
  const tokens = searchTokens(query);
  if (tokens.length === 0) return groups.map((group) => ({ group, models: group.models }));
  const known = new Map(providers.map((provider) => [provider.id, provider]));
  const matches: GroupMatch[] = [];
  for (const group of groups) {
    const fromProvider = providerFields(group.id, group.name, known.get(group.id));
    const models = group.models.filter((model) => everyToken(tokens, [...fromProvider, ...modelFields(model)]));
    if (models.length > 0 || (group.models.length === 0 && everyToken(tokens, fromProvider))) {
      matches.push({ group, models });
    }
  }
  return matches;
}

export interface StarRow {
  star: { provider: string; id: string };
  /** Null when the catalog doesn't list it, or hasn't loaded. The star is kept either way. */
  model: CatalogModel | null;
  providerName: string;
}

export function starRows(
  stars: readonly { provider: string; id: string }[],
  catalog: ModelCatalog | null,
  query: string,
): StarRow[] {
  const tokens = searchTokens(query);
  const providers = new Map((catalog?.providers ?? []).map((provider) => [provider.id, provider]));
  const models = new Map((catalog?.models ?? []).map((model) => [modelOfferingKey(model), model]));
  const rows: StarRow[] = [];
  for (const star of stars) {
    const provider = providers.get(star.provider);
    const providerName = provider?.display_name || star.provider;
    const model = models.get(modelOfferingKey(star)) ?? null;
    const fields = [
      ...providerFields(star.provider, providerName, provider),
      ...(model ? modelFields(model) : [star.id]),
    ];
    if (tokens.length === 0 || everyToken(tokens, fields)) rows.push({ star, model, providerName });
  }
  return rows;
}

// ── What a written value means ──────────────────────────────────────────────

export type ModelResolution =
  | { kind: "template" }
  | { kind: "resolved"; offering: CatalogModel }
  /** A bare ID on several providers. Fabro picks one from what is ready when the run is created. */
  | { kind: "ambiguous"; offerings: CatalogModel[] }
  /** The catalog lists this model, but not under the provider it is paired with. */
  | { kind: "mismatch"; offerings: CatalogModel[] }
  | { kind: "unknown" };

function providerIdOf(catalog: ModelCatalog, text: string): string | undefined {
  return catalog.providers.find((provider) => provider.id === text || provider.aliases?.includes(text))?.id;
}

function within(models: readonly CatalogModel[], providerId: string, selector: string): CatalogModel | undefined {
  const offered = models.filter((model) => model.provider === providerId);
  return offered.find((model) => model.id === selector) ?? offered.find((model) => model.aliases.includes(selector));
}

function everywhere(models: readonly CatalogModel[], selector: string): CatalogModel[] {
  const byId = models.filter((model) => model.id === selector);
  return byId.length > 0 ? byId : models.filter((model) => model.aliases.includes(selector));
}

/** Follows the selector rules in docs/public/core-concepts/models.mdx: slugs before aliases, an explicit provider pins. */
export function resolveModel(catalog: ModelCatalog, model: string, provider: string | undefined): ModelResolution {
  if (hasTemplateSyntax(model) || (provider !== undefined && hasTemplateSyntax(provider))) return { kind: "template" };
  if (provider !== undefined) {
    const hit = within(catalog.models, providerIdOf(catalog, provider) ?? provider, model);
    if (hit) return { kind: "resolved", offering: hit };
    const elsewhere = everywhere(catalog.models, model);
    return elsewhere.length > 0 ? { kind: "mismatch", offerings: elsewhere } : { kind: "unknown" };
  }
  const slash = model.indexOf("/");
  if (slash > 0) {
    const pinned = providerIdOf(catalog, model.slice(0, slash));
    const hit = pinned ? within(catalog.models, pinned, model.slice(slash + 1)) : undefined;
    if (hit) return { kind: "resolved", offering: hit };
  }
  const matches = everywhere(catalog.models, model);
  if (matches.length === 1) return { kind: "resolved", offering: matches[0] };
  return matches.length > 1 ? { kind: "ambiguous", offerings: matches } : { kind: "unknown" };
}

export interface ChoiceSummary {
  /** The friendly name when the catalog knows it, otherwise the ID as written. */
  title: string;
  /** Provider and exact ID. */
  detail: string | null;
  /** Where an inherited value comes from. Null when this element sets it. */
  source: string | null;
  /** The offering to mark as selected in the list. */
  currentKey: string | null;
  setupRequired: boolean;
  notInCatalog: boolean;
  warning: string | null;
}

function providerNameOf(catalog: ModelCatalog | null, provider: string): string {
  const id = catalog ? (providerIdOf(catalog, provider) ?? provider) : provider;
  return catalog?.providers.find((candidate) => candidate.id === id)?.display_name || provider;
}

/**
 * Describes what is written and what it falls back to. It only reads: an imported
 * model the catalog doesn't list is reported, never changed. `catalog` is null
 * until it loads, and then nothing is claimed about the catalog.
 */
export function summarizeChoice(
  catalog: ModelCatalog | null,
  value: ModelChoice,
  inherited: InheritedModel,
): ChoiceSummary {
  const ownModel = value.model || undefined;
  const ownProvider = value.provider || undefined;
  const model = ownModel ?? inherited.model?.value;
  const provider = ownProvider ?? inherited.provider?.value;
  const source = ownModel ? null : (inherited.model?.source ?? (ownProvider ? null : (inherited.provider?.source ?? null)));
  const blank = {
    detail: null,
    source,
    currentKey: null,
    setupRequired: false,
    notInCatalog: false,
    warning: null,
  };

  if (model === undefined) {
    if (provider === undefined) {
      return {
        ...blank,
        title: inherited.stylesheetOpaque ? "Stylesheet or server default" : "Server default",
      };
    }
    // A provider alone runs its default model, per the catalog.
    const id = catalog ? (providerIdOf(catalog, provider) ?? provider) : provider;
    const fallback = catalog?.models.find((candidate) => candidate.provider === id && candidate.default);
    return {
      ...blank,
      title: `${providerNameOf(catalog, provider)} default`,
      detail: fallback?.id ?? null,
      setupRequired: fallback ? !fallback.configured : false,
    };
  }

  const pair = provider === undefined ? model : `${providerNameOf(catalog, provider)} · ${model}`;
  if (!catalog) return { ...blank, title: model, detail: provider === undefined ? null : pair };

  const resolution = resolveModel(catalog, model, provider);
  switch (resolution.kind) {
    case "template":
      return { ...blank, title: model, detail: "Filled in when the run starts" };
    case "resolved": {
      const { offering } = resolution;
      return {
        ...blank,
        title: offering.display_name || offering.id,
        detail: `${providerNameOf(catalog, offering.provider)} · ${model}`,
        currentKey: modelOfferingKey(offering),
        setupRequired: !offering.configured,
      };
    }
    case "ambiguous":
      return { ...blank, title: model, detail: "Provider chosen when the run is created" };
    case "mismatch": {
      const from = ownProvider ? "" : inherited.provider ? ` (${inherited.provider.source.toLowerCase()})` : "";
      return {
        ...blank,
        title: model,
        detail: pair,
        warning: `${model} is not listed under ${provider}${from}. Choose it from the list to set the model and provider together.`,
      };
    }
    case "unknown":
      return { ...blank, title: model, detail: provider === undefined ? null : pair, notInCatalog: true };
  }
}

// ── What a node inherits ────────────────────────────────────────────────────

const STAGE_DEFAULT = "Stage default";
const NODE_DEFAULT = "Node default";
const STYLESHEET = "Model stylesheet";
const WORKFLOW_DEFAULT = "Workflow default";

/** What the model stylesheet writes on this node. Same rules as Petri: higher specificity wins, then the later rule. */
function stylesheetValues(graph: WorkflowGraph, node: WorkflowNode): { values: Map<string, string>; opaque: boolean } {
  const values = new Map<string, string>();
  const text = getAttr(graph.attrs, "model_stylesheet");
  if (!text) return { values, opaque: false };
  if (text.startsWith("@") || hasTemplateSyntax(text)) return { values, opaque: true };
  const parsed = parseStylesheet(text);
  if ("error" in parsed) return { values, opaque: false };
  const classes = nodeClasses(graph, node);
  const shape = shapeOf(effectiveNodeAttributes(graph, node));
  const rules = [...parsed.rules].sort((a, b) => selectorSpecificity(a.selector) - selectorSpecificity(b.selector));
  for (const { selector, declarations } of rules) {
    const matches =
      selector.kind === "universal" ||
      (selector.kind === "shape" && selector.name === shape) ||
      (selector.kind === "class" && classes.includes(selector.name)) ||
      (selector.kind === "id" && selector.name === node.id);
    if (!matches) continue;
    for (const [property, value] of declarations) values.set(property, value);
  }
  return { values, opaque: false };
}

/**
 * The layers below a node's own attributes, in Petri's order: stage and node defaults
 * (which DOT writes onto the node), the stylesheet, then the graph's default_model and
 * default_provider. Past these sits only what the server decides at run time.
 */
export function inheritedNodeModel(graph: WorkflowGraph, node: WorkflowNode): InheritedModel {
  const stages = groupChain(groupsById(graph), node.groupId).reverse();
  const sheet = stylesheetValues(graph, node);

  const inheritedOf = (key: "model" | "provider", graphKey: string): InheritedValue | undefined => {
    for (const stage of stages) {
      const value = getAttr(stage.nodeDefaults, key);
      if (value) return { value, source: STAGE_DEFAULT };
    }
    const fromDefaults = getAttr(graph.nodeDefaults, key);
    if (fromDefaults) return { value: fromDefaults, source: NODE_DEFAULT };
    const fromSheet = sheet.values.get(key);
    if (fromSheet) return { value: fromSheet, source: STYLESHEET };
    const fromGraph = getAttr(graph.attrs, graphKey);
    return fromGraph ? { value: fromGraph, source: WORKFLOW_DEFAULT } : undefined;
  };

  const model = inheritedOf("model", "default_model");
  const provider = inheritedOf("provider", "default_provider");
  return {
    ...(model ? { model } : {}),
    ...(provider ? { provider } : {}),
    ...(sheet.opaque ? { stylesheetOpaque: true } : {}),
  };
}

// ── Writing a choice ────────────────────────────────────────────────────────

export type ModelTarget = { scope: "node"; nodeId: string } | { scope: "graph" };

export function choiceOf(offering: { id: string; provider: string }): ModelChoice {
  return { model: offering.id, provider: offering.provider };
}

/**
 * Sets the model and provider in one change, so one undo restores both and no
 * provider is left paired with a model it doesn't serve. `null` removes both, which
 * returns the element to what it inherits. Returns null for a choice that is already
 * written, so it adds no empty undo step.
 */
export function modelChoiceChange(
  target: ModelTarget,
  choice: ModelChoice | null,
): (context: EditorContext) => EditorChange | null {
  return ({ graph }) => {
    if (!graph) return null;
    if (target.scope === "node") {
      const node = graph.nodes.find((candidate) => candidate.id === target.nodeId);
      if (!node) return null;
      const unchanged =
        (getAttr(node.attrs, "model") || undefined) === (choice?.model || undefined) &&
        (getAttr(node.attrs, "provider") || undefined) === (choice?.provider || undefined);
      if (unchanged) return null;
      return {
        graph: updateNodeAttributes(graph, node.id, { model: choice?.model, provider: choice?.provider }),
      };
    }
    const unchanged =
      (getAttr(graph.attrs, "default_model") || undefined) === (choice?.model || undefined) &&
      (getAttr(graph.attrs, "default_provider") || undefined) === (choice?.provider || undefined);
    if (unchanged) return null;
    return {
      graph: updateGraphAttributes(graph, { default_model: choice?.model, default_provider: choice?.provider }),
    };
  };
}

const CONTROL_CHARACTERS = /\p{Cc}/u;
const MAX_MODEL_ID_LENGTH = 256;
const MAX_PROVIDER_ID_LENGTH = 128;

/** Checks an ID the catalog may not list. Nothing here decides whether it exists. */
export function parseCustomChoice(modelText: string, providerText: string): { choice: ModelChoice } | { error: string } {
  const model = modelText.trim();
  const provider = providerText.trim();
  if (!model) return { error: "Enter a model ID." };
  if (CONTROL_CHARACTERS.test(model) || model.length > MAX_MODEL_ID_LENGTH) {
    return { error: `A model ID is one line of at most ${MAX_MODEL_ID_LENGTH} characters.` };
  }
  if (/\s/.test(provider) || provider.length > MAX_PROVIDER_ID_LENGTH) {
    return { error: `A provider ID has no spaces and at most ${MAX_PROVIDER_ID_LENGTH} characters.` };
  }
  return { choice: provider ? { model, provider } : { model } };
}

// ── Reading the catalog query ───────────────────────────────────────────────

export type PickerCatalogStatus = "loading" | "error" | "ready";

export interface PickerCatalog extends ModelCatalog {
  status: PickerCatalogStatus;
  /** Models loaded but provider names did not, so groups show provider IDs. */
  providersFailed: boolean;
  retrying: boolean;
  retry: () => void;
}

/** A failed refresh keeps the catalog already on screen. Provider names are not worth holding the list back for. */
export function pickerCatalogStatus(queries: {
  hasModels: boolean;
  modelsFailed: boolean;
  hasProviders: boolean;
  providersFailed: boolean;
}): { status: PickerCatalogStatus; providersFailed: boolean } {
  const providersFailed = !queries.hasProviders && queries.providersFailed;
  if (queries.hasModels && (queries.hasProviders || providersFailed)) return { status: "ready", providersFailed };
  if (!queries.hasModels && queries.modelsFailed) return { status: "error", providersFailed };
  return { status: "loading", providersFailed };
}

// ── Keyboard ────────────────────────────────────────────────────────────────

/** Where an arrow, Home or End key moves in the list of stops. Null when it isn't a move. */
export function nextNavIndex(key: string, current: number, count: number): number | null {
  if (count === 0) return null;
  switch (key) {
    case "ArrowDown":
      return current < 0 ? 0 : Math.min(current + 1, count - 1);
    case "ArrowUp":
      return current < 0 ? count - 1 : Math.max(current - 1, 0);
    case "Home":
      return 0;
    case "End":
      return count - 1;
    default:
      return null;
  }
}
