import { queryKeys } from "./query-keys";
import {
  createBrowserEventSource,
  type EventInvalidation,
  type EventPayload,
  type EventSourceLike,
  type MutateFn,
} from "./sse";
import { isRecord } from "./unknown";

export const CROSS_TAB_SSE_CHANNEL = "fabro:sse:v1";
export const HEARTBEAT_MS = 1000;
export const LEADER_STALE_MS = 4000;
export const ELECTION_JITTER_MS = 150;

const MESSAGE_VERSION = 1 as const;
const EVENT_DEDUPE_TTL_MS = 5 * 60 * 1000;
const EVENT_DEDUPE_MAX = 1000;

type TabVisibility = "visible" | "hidden";
type CandidateReason = "hidden-leader" | "stale-leader" | "release" | "no-leader";

interface BaseMessage {
  type: string;
  version: typeof MESSAGE_VERSION;
  tabId: string;
  sentAt: number;
}

interface HelloMessage extends BaseMessage {
  type: "hello";
}

interface HeartbeatMessage extends BaseMessage {
  type: "heartbeat";
  leaderId: string;
  generation: number;
  visibility: TabVisibility;
}

interface CandidateMessage extends BaseMessage {
  type: "candidate";
  candidateId: string;
  candidateGeneration: number;
  visibility: TabVisibility;
  observedLeaderId: string | null;
  observedGeneration: number;
  reason: CandidateReason;
}

interface LeaderChangedMessage extends BaseMessage {
  type: "leader-changed";
  leaderId: string;
  generation: number;
  visibility: TabVisibility;
}

interface ReleaseMessage extends BaseMessage {
  type: "release";
  leaderId: string;
  generation: number;
}

interface ResyncMessage extends BaseMessage {
  type: "resync";
  leaderId: string | null;
  generation: number;
  reason: CandidateReason;
}

interface EventMessage extends BaseMessage {
  type: "event";
  leaderId: string;
  generation: number;
  payload: EventPayload;
}

export type CrossTabSseMessage =
  | HelloMessage
  | HeartbeatMessage
  | CandidateMessage
  | LeaderChangedMessage
  | ReleaseMessage
  | ResyncMessage
  | EventMessage;

export interface BroadcastChannelLike {
  onmessage: ((event: { data: unknown }) => void) | null;
  postMessage(message: CrossTabSseMessage): void;
  close(): void;
}

interface TimingOptions {
  heartbeatMs: number;
  leaderStaleMs: number;
  electionJitterMs: number;
}

export interface CrossTabSseCoordinatorOptions {
  tabId?: string;
  channelFactory?: (name: string) => BroadcastChannelLike;
  eventSourceFactory?: (url: string) => EventSourceLike;
  getVisibility?: () => TabVisibility;
  addVisibilityChangeListener?: (handler: () => void) => () => void;
  addPagehideListener?: (handler: () => void) => () => void;
  now?: () => number;
  timing?: Partial<TimingOptions>;
}

interface SubscribeOptions<TPayload extends EventPayload> {
  subscriptionKey: string;
  mutate: MutateFn;
  resolveInvalidation: (payload: TPayload) => EventInvalidation;
  resyncKeys: () => string[];
  fallbackSubscribe: () => () => void;
  eventSourceFactory?: (url: string) => EventSourceLike;
  debounceMs?: number;
}

export interface SubscribeToCrossTabSseOptions<TPayload extends EventPayload>
  extends SubscribeOptions<TPayload> {
  coordinator?: CrossTabSseCoordinator;
}

interface FallbackEntry {
  count: number;
  subscribe: () => () => void;
  cleanup?: () => void;
}

interface LocalSubscription {
  refcount: number;
  mutators: Map<MutateFn, number>;
  fallbacks: Map<MutateFn, FallbackEntry>;
  pendingKeys: Set<string>;
  debounceTimer: ReturnType<typeof setTimeout> | null;
  debounceMs: number;
  resolveInvalidation: (payload: EventPayload) => EventInvalidation;
  resyncKeys: () => string[];
}

interface LeaderState {
  leaderId: string;
  generation: number;
  visibility: TabVisibility;
  lastSeen: number;
}

class RecentEventCache {
  private readonly seen = new Map<string, number>();

  constructor(
    private readonly maxSize: number,
    private readonly ttlMs: number,
  ) {}

  remember(key: string | undefined, now: number): boolean {
    if (!key) return true;
    this.prune(now);
    if (this.seen.has(key)) return false;

    this.seen.set(key, now);
    while (this.seen.size > this.maxSize) {
      const oldest = this.seen.keys().next().value;
      if (oldest === undefined) break;
      this.seen.delete(oldest);
    }
    return true;
  }

  private prune(now: number) {
    for (const [key, seenAt] of this.seen) {
      if (now - seenAt > this.ttlMs) {
        this.seen.delete(key);
      }
    }
  }
}

export class CrossTabSseCoordinator {
  readonly tabId: string;

  private readonly channelFactory: (name: string) => BroadcastChannelLike;
  private readonly getVisibility: () => TabVisibility;
  private readonly addVisibilityChangeListener: (handler: () => void) => () => void;
  private readonly addPagehideListener: (handler: () => void) => () => void;
  private readonly now: () => number;
  private readonly timing: TimingOptions;
  private readonly recentEvents = new RecentEventCache(EVENT_DEDUPE_MAX, EVENT_DEDUPE_TTL_MS);
  private readonly subscriptions = new Map<string, LocalSubscription>();
  private readonly candidates = new Map<string, CandidateMessage>();

  private sourceFactory: (url: string) => EventSourceLike;
  private channel: BroadcastChannelLike | null = null;
  private source: EventSourceLike | null = null;
  private initialized = false;
  private coordinationUnavailable = false;
  private fallbackMode = false;
  private degradingToFallback = false;
  private isLeader = false;
  private leader: LeaderState | null = null;
  private generation = 0;
  private ownCandidate: CandidateMessage | null = null;
  private candidateTimer: ReturnType<typeof setTimeout> | null = null;
  private noLeaderTimer: ReturnType<typeof setTimeout> | null = null;
  private heartbeatTimer: ReturnType<typeof setInterval> | null = null;
  private leaderCheckTimer: ReturnType<typeof setInterval> | null = null;
  private removeVisibilityListener: (() => void) | null = null;
  private removePagehideListener: (() => void) | null = null;
  private sourceFactoryLocked = false;

  constructor(options: CrossTabSseCoordinatorOptions = {}) {
    this.tabId = options.tabId ?? createTabId();
    this.channelFactory = options.channelFactory ?? createBrowserBroadcastChannel;
    this.sourceFactory = options.eventSourceFactory ?? createBrowserEventSource;
    this.getVisibility = options.getVisibility ?? getBrowserVisibility;
    this.addVisibilityChangeListener =
      options.addVisibilityChangeListener ?? addBrowserVisibilityChangeListener;
    this.addPagehideListener = options.addPagehideListener ?? addBrowserPagehideListener;
    this.now = options.now ?? Date.now;
    this.timing = {
      heartbeatMs: options.timing?.heartbeatMs ?? HEARTBEAT_MS,
      leaderStaleMs: options.timing?.leaderStaleMs ?? LEADER_STALE_MS,
      electionJitterMs: options.timing?.electionJitterMs ?? ELECTION_JITTER_MS,
    };
  }

  subscribe<TPayload extends EventPayload>(options: SubscribeOptions<TPayload>): () => void {
    if (options.eventSourceFactory && !this.source && !this.sourceFactoryLocked) {
      this.sourceFactory = options.eventSourceFactory;
      this.sourceFactoryLocked = true;
    }

    if (this.coordinationUnavailable) {
      return options.fallbackSubscribe();
    }

    if (!this.initialized && !this.initialize()) {
      this.coordinationUnavailable = true;
      return options.fallbackSubscribe();
    }

    const subscription = this.addLocalSubscription(options);
    if (this.fallbackMode) {
      this.startFallbacksFor(subscription);
    } else {
      this.ensureLeadershipProgress();
    }

    let active = true;
    return () => {
      if (!active) return;
      active = false;
      this.removeLocalSubscription(options.subscriptionKey, options.mutate);
    };
  }

  close() {
    this.releaseLeadership({ broadcast: false, resync: false });
    this.clearCandidate();
    this.clearNoLeaderTimer();
    this.shutdownTimersAndChannel();
    this.closeFallbacks();
    this.subscriptions.clear();
    this.leader = null;
    this.initialized = false;
    this.fallbackMode = false;
    this.sourceFactoryLocked = false;
  }

  private initialize(): boolean {
    try {
      this.channel = this.channelFactory(CROSS_TAB_SSE_CHANNEL);
    } catch {
      this.channel = null;
      return false;
    }

    this.channel.onmessage = (event) => this.handleMessage(event.data);
    this.initialized = true;
    this.removeVisibilityListener = this.addVisibilityChangeListener(() => {
      this.handleVisibilityChange();
    });
    this.removePagehideListener = this.addPagehideListener(() => {
      this.handlePagehide();
    });
    this.leaderCheckTimer = setInterval(() => {
      this.checkLeaderFreshness();
    }, this.timing.heartbeatMs);

    return this.post({ type: "hello", version: MESSAGE_VERSION, tabId: this.tabId, sentAt: this.now() });
  }

  private addLocalSubscription<TPayload extends EventPayload>(
    options: SubscribeOptions<TPayload>,
  ): LocalSubscription {
    let subscription = this.subscriptions.get(options.subscriptionKey);
    if (!subscription) {
      subscription = {
        refcount: 0,
        mutators: new Map(),
        fallbacks: new Map(),
        pendingKeys: new Set(),
        debounceTimer: null,
        debounceMs: options.debounceMs ?? 300,
        resolveInvalidation: options.resolveInvalidation as (payload: EventPayload) => EventInvalidation,
        resyncKeys: options.resyncKeys,
      };
      this.subscriptions.set(options.subscriptionKey, subscription);
    } else {
      subscription.resolveInvalidation =
        options.resolveInvalidation as (payload: EventPayload) => EventInvalidation;
      subscription.resyncKeys = options.resyncKeys;
      subscription.debounceMs = options.debounceMs ?? subscription.debounceMs;
    }

    subscription.refcount += 1;
    subscription.mutators.set(
      options.mutate,
      (subscription.mutators.get(options.mutate) ?? 0) + 1,
    );

    const fallback = subscription.fallbacks.get(options.mutate);
    if (fallback) {
      fallback.count += 1;
      fallback.subscribe = options.fallbackSubscribe;
    } else {
      subscription.fallbacks.set(options.mutate, {
        count: 1,
        subscribe: options.fallbackSubscribe,
      });
    }

    return subscription;
  }

  private removeLocalSubscription(subscriptionKey: string, mutate: MutateFn) {
    const subscription = this.subscriptions.get(subscriptionKey);
    if (!subscription) return;

    const mutateCount = subscription.mutators.get(mutate) ?? 0;
    if (mutateCount <= 1) {
      subscription.mutators.delete(mutate);
    } else {
      subscription.mutators.set(mutate, mutateCount - 1);
    }

    const fallback = subscription.fallbacks.get(mutate);
    if (fallback) {
      fallback.count -= 1;
      if (fallback.count <= 0) {
        fallback.cleanup?.();
        subscription.fallbacks.delete(mutate);
      }
    }

    subscription.refcount -= 1;
    if (subscription.refcount <= 0) {
      if (subscription.debounceTimer) {
        clearTimeout(subscription.debounceTimer);
      }
      this.subscriptions.delete(subscriptionKey);
    }

    if (this.subscriptions.size === 0) {
      this.releaseLeadership({ broadcast: true, resync: false });
      this.clearCandidate();
      this.clearNoLeaderTimer();
      this.shutdownTimersAndChannel();
      this.initialized = false;
      this.fallbackMode = false;
      this.sourceFactoryLocked = false;
    }
  }

  private handleMessage(data: unknown) {
    const message = parseMessage(data);
    if (!message || message.tabId === this.tabId) return;

    switch (message.type) {
      case "hello":
        if (this.isLeader) this.sendHeartbeat();
        break;
      case "heartbeat":
        this.handleLeaderAnnouncement(message, { resyncOnChange: true });
        break;
      case "candidate":
        this.handleCandidate(message);
        break;
      case "leader-changed":
        this.handleLeaderAnnouncement(message, { resyncOnChange: true });
        break;
      case "release":
        this.handleRelease(message);
        break;
      case "resync":
        this.handleResync(message);
        break;
      case "event":
        this.handleBroadcastEvent(message);
        break;
    }
  }

  private handleLeaderAnnouncement(
    message: HeartbeatMessage | LeaderChangedMessage,
    { resyncOnChange }: { resyncOnChange: boolean },
  ) {
    const incoming: LeaderState = {
      leaderId: message.leaderId,
      generation: message.generation,
      visibility: message.visibility,
      lastSeen: this.now(),
    };

    if (this.isLeader && incoming.leaderId !== this.tabId) {
      const own: LeaderState = {
        leaderId: this.tabId,
        generation: this.generation,
        visibility: this.currentVisibility(),
        lastSeen: this.now(),
      };
      if (
        incoming.generation > own.generation ||
        (incoming.generation === own.generation && leaderHasHigherPriority(incoming, own))
      ) {
        this.releaseLeadership({ broadcast: false, resync: true });
      } else {
        return;
      }
    }

    const previous = this.leader;
    if (this.ownCandidate && incoming.generation === this.ownCandidate.candidateGeneration) {
      const sawIncomingCandidate = this.candidates.has(
        `${incoming.generation}:${incoming.leaderId}`,
      );
      const shouldAcceptFreshVisibleLeader =
        this.ownCandidate.reason === "no-leader" &&
        incoming.visibility === "visible" &&
        !sawIncomingCandidate;

      if (
        !shouldAcceptFreshVisibleLeader &&
        !leaderHasHigherPriority(incoming, leaderStateForCandidate(this.ownCandidate, this.now()))
      ) {
        return;
      }
    }

    if (!this.shouldAcceptLeader(incoming)) return;

    this.leader = incoming;
    this.clearNoLeaderTimer();
    this.generation = Math.max(this.generation, incoming.generation);
    if (this.ownCandidate && incoming.generation >= this.ownCandidate.candidateGeneration) {
      this.clearCandidate();
    }

    const changed =
      !previous ||
      previous.leaderId !== incoming.leaderId ||
      previous.generation !== incoming.generation;

    if (changed && resyncOnChange) {
      this.resyncAll();
    }

    if (incoming.visibility === "hidden" && this.currentVisibility() === "visible") {
      this.enterCandidacy("hidden-leader", incoming);
    }
  }

  private handleCandidate(message: CandidateMessage) {
    this.candidates.set(candidateKey(message), message);

    if (
      this.isLeader &&
      message.observedLeaderId === this.tabId &&
      message.observedGeneration >= this.generation
    ) {
      this.releaseLeadership({ broadcast: true, resync: true });
    }

    if (
      this.ownCandidate &&
      message.candidateGeneration === this.ownCandidate.candidateGeneration &&
      candidateHasHigherPriority(message, this.ownCandidate)
    ) {
      this.clearCandidate();
    }
  }

  private handleRelease(message: ReleaseMessage) {
    const current = this.leader;
    if (
      current &&
      message.leaderId === current.leaderId &&
      message.generation >= current.generation
    ) {
      this.leader = null;
      this.generation = Math.max(this.generation, message.generation);
      this.enterCandidacy("release", {
        leaderId: message.leaderId,
        generation: message.generation,
        visibility: "hidden",
        lastSeen: this.now(),
      });
    }
  }

  private handleResync(message: ResyncMessage) {
    if (this.leader && message.generation < this.leader.generation) return;
    this.resyncAll();
  }

  private handleBroadcastEvent(message: EventMessage) {
    if (!this.isCurrentLeader(message.leaderId, message.generation)) return;
    if (!this.recentEvents.remember(eventDedupeKey(message.payload), this.now())) return;
    this.dispatchPayload(message.payload);
  }

  private ensureLeadershipProgress() {
    if (this.subscriptions.size === 0 || this.isLeader || this.ownCandidate) return;

    if (!this.leader) {
      this.scheduleNoLeaderCandidacy();
      return;
    }

    if (this.leader.visibility === "hidden" && this.currentVisibility() === "visible") {
      this.enterCandidacy("hidden-leader", this.leader);
    }
  }

  private checkLeaderFreshness() {
    if (this.subscriptions.size === 0 || this.fallbackMode) return;
    if (this.isLeader) return;

    const current = this.leader;
    if (!current) {
      this.scheduleNoLeaderCandidacy();
      return;
    }

    if (this.now() - current.lastSeen > this.timing.leaderStaleMs) {
      this.leader = null;
      this.generation = Math.max(this.generation, current.generation);
      this.resyncAll();
      this.enterCandidacy("stale-leader", current);
      return;
    }

    if (current.visibility === "hidden" && this.currentVisibility() === "visible") {
      this.enterCandidacy("hidden-leader", current);
    }
  }

  private enterCandidacy(reason: CandidateReason, observedLeader: LeaderState | null = this.leader) {
    if (this.subscriptions.size === 0 || this.fallbackMode) return;

    if (
      reason === "no-leader" &&
      this.leader &&
      this.leader.visibility === "visible" &&
      this.now() - this.leader.lastSeen <= this.timing.leaderStaleMs
    ) {
      return;
    }

    const observedGeneration = observedLeader?.generation ?? this.generation;
    const candidateGeneration = observedGeneration + 1;
    if (
      this.ownCandidate &&
      this.ownCandidate.candidateGeneration >= candidateGeneration
    ) {
      return;
    }

    this.clearCandidate();
    this.clearNoLeaderTimer();
    const candidate: CandidateMessage = {
      type: "candidate",
      version: MESSAGE_VERSION,
      tabId: this.tabId,
      sentAt: this.now(),
      candidateId: this.tabId,
      candidateGeneration,
      visibility: this.currentVisibility(),
      observedLeaderId: observedLeader?.leaderId ?? null,
      observedGeneration,
      reason,
    };

    this.ownCandidate = candidate;
    this.candidates.set(candidateKey(candidate), candidate);
    if (!this.post(candidate)) return;
    this.candidateTimer = setTimeout(() => {
      this.completeCandidacy(candidate);
    }, this.timing.electionJitterMs);
  }

  private completeCandidacy(candidate: CandidateMessage) {
    if (this.ownCandidate !== candidate || this.fallbackMode) return;

    for (const other of this.candidates.values()) {
      if (
        other.candidateGeneration === candidate.candidateGeneration &&
        candidateHasHigherPriority(other, candidate)
      ) {
        this.clearCandidate();
        return;
      }
    }

    if (
      this.leader &&
      this.now() - this.leader.lastSeen <= this.timing.leaderStaleMs
    ) {
      if (this.leader.generation > candidate.candidateGeneration) {
        this.clearCandidate();
        return;
      }
      if (
        this.leader.generation === candidate.candidateGeneration &&
        leaderHasHigherPriority(this.leader, leaderStateForCandidate(candidate, this.now()))
      ) {
        this.clearCandidate();
        return;
      }
    }

    this.becomeLeader(candidate.candidateGeneration);
  }

  private becomeLeader(generation: number) {
    this.clearCandidate();
    this.closeSource();
    this.isLeader = true;
    this.generation = generation;
    this.leader = {
      leaderId: this.tabId,
      generation,
      visibility: this.currentVisibility(),
      lastSeen: this.now(),
    };

    const source = this.sourceFactory(queryKeys.system.attach());
    this.source = source;
    source.onmessage = (message) => {
      this.handleLeaderEventSourceMessage(message.data);
    };

    const announced = this.post({
      type: "leader-changed",
      version: MESSAGE_VERSION,
      tabId: this.tabId,
      sentAt: this.now(),
      leaderId: this.tabId,
      generation,
      visibility: this.currentVisibility(),
    });
    if (!announced) return;
    this.startHeartbeat();
    this.resyncAll();
  }

  private handleLeaderEventSourceMessage(data: string) {
    if (!this.isLeader) return;

    let payload: EventPayload;
    try {
      payload = JSON.parse(data) as EventPayload;
    } catch {
      return;
    }

    if (!this.recentEvents.remember(eventDedupeKey(payload), this.now())) return;
    this.dispatchPayload(payload);
    this.post({
      type: "event",
      version: MESSAGE_VERSION,
      tabId: this.tabId,
      sentAt: this.now(),
      leaderId: this.tabId,
      generation: this.generation,
      payload,
    });
  }

  private dispatchPayload(payload: EventPayload) {
    for (const subscription of this.subscriptions.values()) {
      const invalidation = subscription.resolveInvalidation(payload);
      this.queueInvalidations(subscription, invalidation.keys, {
        immediate: invalidation.immediate,
      });
    }
  }

  private queueInvalidations(
    subscription: LocalSubscription,
    keys: string[],
    { immediate = false }: { immediate?: boolean } = {},
  ) {
    if (keys.length === 0) return;
    for (const key of keys) {
      subscription.pendingKeys.add(key);
    }

    if (immediate || subscription.debounceMs <= 0) {
      this.flushInvalidations(subscription);
      return;
    }

    if (subscription.debounceTimer) {
      clearTimeout(subscription.debounceTimer);
    }
    subscription.debounceTimer = setTimeout(() => {
      subscription.debounceTimer = null;
      this.flushInvalidations(subscription);
    }, subscription.debounceMs);
  }

  private flushInvalidations(subscription: LocalSubscription) {
    if (subscription.pendingKeys.size === 0) return;
    const keys = [...subscription.pendingKeys];
    subscription.pendingKeys.clear();

    for (const mutator of subscription.mutators.keys()) {
      for (const key of keys) {
        void mutator(key);
      }
    }
  }

  private resyncAll() {
    for (const subscription of this.subscriptions.values()) {
      this.queueInvalidations(subscription, subscription.resyncKeys(), { immediate: true });
    }
  }

  private handleVisibilityChange() {
    if (this.isLeader) {
      this.sendHeartbeat();
    }

    if (this.currentVisibility() === "visible") {
      this.resyncAll();
      if (this.leader?.visibility === "hidden") {
        this.enterCandidacy("hidden-leader", this.leader);
      }
    }
  }

  private handlePagehide() {
    this.releaseLeadership({ broadcast: true, resync: false });
    this.clearCandidate();
    this.clearNoLeaderTimer();
  }

  private startHeartbeat() {
    if (this.heartbeatTimer) {
      clearInterval(this.heartbeatTimer);
    }
    if (!this.sendHeartbeat()) return;
    this.heartbeatTimer = setInterval(() => {
      this.sendHeartbeat();
    }, this.timing.heartbeatMs);
  }

  private sendHeartbeat(): boolean {
    if (!this.isLeader) return false;
    const visibility = this.currentVisibility();
    this.leader = {
      leaderId: this.tabId,
      generation: this.generation,
      visibility,
      lastSeen: this.now(),
    };
    return this.post({
      type: "heartbeat",
      version: MESSAGE_VERSION,
      tabId: this.tabId,
      sentAt: this.now(),
      leaderId: this.tabId,
      generation: this.generation,
      visibility,
    });
  }

  private releaseLeadership({
    broadcast,
    resync,
  }: {
    broadcast: boolean;
    resync: boolean;
  }) {
    if (!this.isLeader && !this.source) return;

    const generation = this.generation;
    this.closeSource();
    this.isLeader = false;
    if (this.heartbeatTimer) {
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = null;
    }
    this.leader = null;

    if (broadcast) {
      const released = this.post({
        type: "release",
        version: MESSAGE_VERSION,
        tabId: this.tabId,
        sentAt: this.now(),
        leaderId: this.tabId,
        generation,
      });
      if (!released) return;
      this.post({
        type: "resync",
        version: MESSAGE_VERSION,
        tabId: this.tabId,
        sentAt: this.now(),
        leaderId: this.tabId,
        generation,
        reason: "release",
      });
    }
    if (resync) this.resyncAll();
  }

  private closeSource() {
    if (!this.source) return;
    this.source.close();
    this.source = null;
  }

  private clearCandidate() {
    if (this.candidateTimer) {
      clearTimeout(this.candidateTimer);
      this.candidateTimer = null;
    }
    this.ownCandidate = null;
  }

  private scheduleNoLeaderCandidacy() {
    if (
      this.noLeaderTimer ||
      this.ownCandidate ||
      this.isLeader ||
      this.leader ||
      this.subscriptions.size === 0 ||
      this.fallbackMode
    ) {
      return;
    }

    this.noLeaderTimer = setTimeout(() => {
      this.noLeaderTimer = null;
      if (!this.leader && !this.isLeader) {
        this.enterCandidacy("no-leader");
      }
    }, this.timing.electionJitterMs);
  }

  private clearNoLeaderTimer() {
    if (!this.noLeaderTimer) return;
    clearTimeout(this.noLeaderTimer);
    this.noLeaderTimer = null;
  }

  private shutdownTimersAndChannel() {
    this.clearNoLeaderTimer();
    if (this.heartbeatTimer) {
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = null;
    }
    if (this.leaderCheckTimer) {
      clearInterval(this.leaderCheckTimer);
      this.leaderCheckTimer = null;
    }
    this.removeVisibilityListener?.();
    this.removeVisibilityListener = null;
    this.removePagehideListener?.();
    this.removePagehideListener = null;
    if (this.channel) {
      this.channel.close();
      this.channel = null;
    }
  }

  private post(message: CrossTabSseMessage): boolean {
    if (!this.channel) {
      this.degradeToFallback();
      return false;
    }
    try {
      this.channel.postMessage(message);
      return true;
    } catch {
      this.degradeToFallback();
      return false;
    }
  }

  private degradeToFallback() {
    if (this.degradingToFallback || this.fallbackMode) return;
    this.degradingToFallback = true;
    this.releaseLeadership({ broadcast: false, resync: false });
    this.clearCandidate();
    this.shutdownTimersAndChannel();
    this.initialized = false;
    this.fallbackMode = true;
    this.coordinationUnavailable = true;
    for (const subscription of this.subscriptions.values()) {
      this.startFallbacksFor(subscription);
    }
    this.degradingToFallback = false;
  }

  private startFallbacksFor(subscription: LocalSubscription) {
    for (const fallback of subscription.fallbacks.values()) {
      if (!fallback.cleanup) {
        fallback.cleanup = fallback.subscribe();
      }
    }
  }

  private closeFallbacks() {
    for (const subscription of this.subscriptions.values()) {
      for (const fallback of subscription.fallbacks.values()) {
        fallback.cleanup?.();
        fallback.cleanup = undefined;
      }
    }
  }

  private shouldAcceptLeader(incoming: LeaderState): boolean {
    if (!this.leader) return true;
    if (incoming.generation > this.leader.generation) return true;
    if (incoming.generation < this.leader.generation) return false;
    if (incoming.leaderId === this.leader.leaderId) return true;
    return leaderHasHigherPriority(incoming, this.leader);
  }

  private isCurrentLeader(leaderId: string, generation: number): boolean {
    return Boolean(
      this.leader &&
        this.leader.leaderId === leaderId &&
        this.leader.generation === generation,
    );
  }

  private currentVisibility(): TabVisibility {
    return this.getVisibility() === "hidden" ? "hidden" : "visible";
  }
}

const defaultCoordinator = new CrossTabSseCoordinator();

export function createCrossTabSseCoordinator(options: CrossTabSseCoordinatorOptions = {}) {
  return new CrossTabSseCoordinator(options);
}

export function subscribeToCrossTabSse<TPayload extends EventPayload>({
  coordinator = defaultCoordinator,
  ...options
}: SubscribeToCrossTabSseOptions<TPayload>): () => void {
  return coordinator.subscribe(options);
}

function createBrowserBroadcastChannel(name: string): BroadcastChannelLike {
  if (typeof BroadcastChannel === "undefined") {
    throw new Error("BroadcastChannel is unavailable");
  }
  return new BroadcastChannel(name);
}

function createTabId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `tab-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

function getBrowserVisibility(): TabVisibility {
  if (typeof document === "undefined") return "visible";
  return document.visibilityState === "visible" ? "visible" : "hidden";
}

function addBrowserVisibilityChangeListener(handler: () => void): () => void {
  if (typeof document === "undefined") return () => {};
  document.addEventListener("visibilitychange", handler);
  return () => document.removeEventListener("visibilitychange", handler);
}

function addBrowserPagehideListener(handler: () => void): () => void {
  if (typeof window === "undefined") return () => {};
  window.addEventListener("pagehide", handler);
  return () => window.removeEventListener("pagehide", handler);
}

function parseMessage(data: unknown): CrossTabSseMessage | undefined {
  if (!isRecord(data)) return undefined;
  if (data.version !== MESSAGE_VERSION) return undefined;
  const type = data.type;
  const tabId = data.tabId;
  const sentAt = data.sentAt;
  if (typeof type !== "string") return undefined;
  if (typeof tabId !== "string") return undefined;
  if (typeof sentAt !== "number") return undefined;
  const base = { tabId, sentAt };

  switch (type) {
    case "hello":
      return baseMessage(base, "hello");
    case "heartbeat": {
      const { leaderId, generation, visibility } = data;
      if (typeof leaderId === "string" && typeof generation === "number" && isVisibility(visibility)) {
        return {
          ...baseMessage(base, "heartbeat"),
          leaderId,
          generation,
          visibility,
        };
      }
      return undefined;
    }
    case "candidate": {
      const {
        candidateId,
        candidateGeneration,
        visibility,
        observedLeaderId,
        observedGeneration,
        reason,
      } = data;
      if (
        typeof candidateId === "string" &&
        typeof candidateGeneration === "number" &&
        isVisibility(visibility) &&
        (typeof observedLeaderId === "string" || observedLeaderId === null) &&
        typeof observedGeneration === "number" &&
        isCandidateReason(reason)
      ) {
        const normalizedObservedLeaderId =
          typeof observedLeaderId === "string" ? observedLeaderId : null;
        return {
          ...baseMessage(base, "candidate"),
          candidateId,
          candidateGeneration,
          visibility,
          observedLeaderId: normalizedObservedLeaderId,
          observedGeneration,
          reason,
        };
      }
      return undefined;
    }
    case "leader-changed": {
      const { leaderId, generation, visibility } = data;
      if (typeof leaderId === "string" && typeof generation === "number" && isVisibility(visibility)) {
        return {
          ...baseMessage(base, "leader-changed"),
          leaderId,
          generation,
          visibility,
        };
      }
      return undefined;
    }
    case "release": {
      const { leaderId, generation } = data;
      if (typeof leaderId === "string" && typeof generation === "number") {
        return {
          ...baseMessage(base, "release"),
          leaderId,
          generation,
        };
      }
      return undefined;
    }
    case "resync": {
      const { leaderId, generation, reason } = data;
      if (
        (typeof leaderId === "string" || leaderId === null) &&
        typeof generation === "number" &&
        isCandidateReason(reason)
      ) {
        const normalizedLeaderId = typeof leaderId === "string" ? leaderId : null;
        return {
          ...baseMessage(base, "resync"),
          leaderId: normalizedLeaderId,
          generation,
          reason,
        };
      }
      return undefined;
    }
    case "event": {
      const { leaderId, generation, payload } = data;
      if (typeof leaderId === "string" && typeof generation === "number" && isRecord(payload)) {
        return {
          ...baseMessage(base, "event"),
          leaderId,
          generation,
          payload,
        };
      }
      return undefined;
    }
    default:
      return undefined;
  }
}

function baseMessage<TType extends CrossTabSseMessage["type"]>(
  data: {
    tabId: string;
    sentAt: number;
  },
  type: TType,
) {
  return {
    type,
    version: MESSAGE_VERSION,
    tabId: data.tabId,
    sentAt: data.sentAt,
  };
}

function isVisibility(value: unknown): value is TabVisibility {
  return value === "visible" || value === "hidden";
}

function isCandidateReason(value: unknown): value is CandidateReason {
  return (
    value === "hidden-leader" ||
    value === "stale-leader" ||
    value === "release" ||
    value === "no-leader"
  );
}

function candidateHasHigherPriority(candidate: CandidateMessage, other: CandidateMessage): boolean {
  if (candidate.visibility !== other.visibility) return candidate.visibility === "visible";
  return candidate.candidateId < other.candidateId;
}

function leaderHasHigherPriority(candidate: LeaderState, other: LeaderState): boolean {
  if (candidate.visibility !== other.visibility) return candidate.visibility === "visible";
  return candidate.leaderId < other.leaderId;
}

function leaderStateForCandidate(candidate: CandidateMessage, lastSeen: number): LeaderState {
  return {
    leaderId: candidate.candidateId,
    generation: candidate.candidateGeneration,
    visibility: candidate.visibility,
    lastSeen,
  };
}

function candidateKey(candidate: CandidateMessage): string {
  return `${candidate.candidateGeneration}:${candidate.candidateId}`;
}

function eventDedupeKey(payload: EventPayload): string | undefined {
  if (typeof payload.id === "string" && payload.id.length > 0) {
    return payload.id;
  }

  const runId = typeof payload.run_id === "string" ? payload.run_id : undefined;
  const seq = typeof payload.seq === "number" ? payload.seq : undefined;
  const event = typeof payload.event === "string" ? payload.event : undefined;
  if (runId && seq != null && event) {
    return `${runId}:${seq}:${event}`;
  }
  return undefined;
}
