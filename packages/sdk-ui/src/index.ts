export { mountGameSettings } from './game-settings';
export type { GameUiDescriptor, GameModuleList } from './game-settings';
export { getUiTheme, mountReactView, navigateUi, publishUiSelection, setUiTheme } from './react-view';
export type { UiNavigationIcon, UiSelection, UiThemeName } from './react-view';
export { presentUiError, renderUiError } from './error-presentation';
export type { UiErrorContext, UiErrorKind, UiErrorPresentation } from './error-presentation';
export {
  SOURCE_AUTH_GUIDES,
  SOURCE_NAMES,
  copyTextToClipboard,
  getSourceAuthGuide,
  getSourceDisplayName,
  isRemoteSourceId,
  validateSourceCredentialInput,
} from './source-auth';
export type { RemoteSourceId, SourceAuthGuide } from './source-auth';
import type { CommandName, CommandParams, CommandResult } from '@mintcat/sdk-ts';
import type * as ReactRuntime from 'react';
import type * as ReactDOMRuntime from 'react-dom/client';
import type * as AntdRuntime from 'antd';
import type * as IconsRuntime from '@ant-design/icons';
import type { createPortal as CreatePortal, flushSync as FlushSync } from 'react-dom';

export interface Disposable { dispose(): void }
export class Disposables implements Disposable {
  private items: Disposable[] = [];
  private closed = false;
  add<T extends Disposable>(item: T): T {
    if (!item || typeof item.dispose !== 'function') throw new Error('UI registration must return a Disposable');
    if (this.closed) item.dispose(); else this.items.push(item);
    return item;
  }
  dispose(): void {
    if (this.closed) return;
    this.closed = true;
    const errors: unknown[] = [];
    for (const item of this.items.splice(0).reverse()) {
      try { item.dispose(); } catch (error) { errors.push(error); }
    }
    if (errors.length) console.error('Module cleanup failed', errors);
  }
}
export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export interface SessionState { schemaVersion: number; value: Json }
export class SessionStore {
  private states = new Map<string, SessionState>();
  constructor(private storage?: Storage, private key = 'mintcat.workbench.session.v1') {
    try {
      const saved: unknown = JSON.parse(storage?.getItem(key) ?? '{}');
      if (saved && typeof saved === 'object') {
        for (const [id, state] of Object.entries(saved)) {
          if (state && typeof state === 'object' && typeof state.schemaVersion === 'number' && 'value' in state) {
            this.states.set(id, structuredClone(state));
          }
        }
      }
    } catch { /* Corrupt UI state never prevents rescue or daemon connection. */ }
  }
  get<T extends Json>(id: string, schemaVersion: number, fallback: T): T {
    const state = this.states.get(id);
    return structuredClone(state?.schemaVersion === schemaVersion ? state.value as T : fallback);
  }
  snapshot(): Record<string, SessionState> {
    return structuredClone(Object.fromEntries(this.states));
  }
  set(id: string, schemaVersion: number, value: Json): void {
    const serialized = JSON.stringify(value);
    if (serialized.length > 256 * 1024) throw new Error('UI session state exceeds limit');
    this.states.set(id, { schemaVersion, value: JSON.parse(serialized) as Json });
    try { this.storage?.setItem(this.key, JSON.stringify(Object.fromEntries(this.states))); }
    catch { /* Session state remains available in this WebView when storage is full. */ }
  }
}
export class QueryCache {
  private values = new Map<string, { value: unknown; expires: number }>();
  private pending = new Map<string, Promise<unknown>>();
  private epochs = new Map<string, number>();
  async get<T>(key: string, load: () => Promise<T>, ttlMs = 2_000): Promise<T> {
    const existing = this.values.get(key);
    if (existing && existing.expires > Date.now()) return existing.value as T;
    let request = this.pending.get(key) as Promise<T> | undefined;
    if (!request) {
      const epoch = this.epochs.get(key) ?? 0;
      request = load().then(value => {
        if ((this.epochs.get(key) ?? 0) === epoch) this.values.set(key, { value, expires: Date.now() + ttlMs });
        return value;
      }).finally(() => { if (this.pending.get(key) === request) this.pending.delete(key); });
      this.pending.set(key, request);
    }
    return request;
  }
  invalidate(prefix: string): void {
    for (const key of new Set([...this.values.keys(), ...this.pending.keys()])) {
      if (!key.startsWith(prefix)) continue;
      this.values.delete(key);
      this.pending.delete(key);
      this.epochs.set(key, (this.epochs.get(key) ?? 0) + 1);
    }
  }
  releasePending(prefix: string): void {
    for (const key of this.pending.keys()) {
      if (!key.startsWith(prefix)) continue;
      this.pending.delete(key);
      this.epochs.set(key, (this.epochs.get(key) ?? 0) + 1);
    }
  }
}
export interface UiContext {
  /** Local page state/cache namespace only; application requests use the authenticated human session. */
  readonly moduleId: string;
  readonly signal: AbortSignal;
  readonly toolbar?: HTMLElement;
  own<T extends Disposable>(disposable: T): T;
  onInventoryChanged(listener: () => void | Promise<void>): Disposable;
  call<M extends CommandName>(method: M, params: CommandParams[M], idempotencyKey?: string): Promise<CommandResult[M]>;
  request<T>(method: string, params: Record<string, unknown>, idempotencyKey?: string): Promise<T>;
  query<T>(key: string, load: () => Promise<T>, ttlMs?: number): Promise<T>;
  invalidate(prefix: string): void;
  state<T extends Json>(key: string, version: number, fallback: T): T;
  saveState(key: string, version: number, value: Json): void;
  dirty(value: boolean): void;
  notify(message: string): void;
  guard<T extends unknown[]>(action: (...args: T) => void | Promise<void>): (...args: T) => void;
  interval(action: () => void | Promise<void>, milliseconds: number): Disposable;
  readonly kit: typeof kit;
}
export type PageMount = (root: HTMLElement, context: UiContext) => Disposable | Promise<Disposable>;
export const kit = Object.freeze({
  element<K extends keyof HTMLElementTagNameMap>(tag: K, text?: string): HTMLElementTagNameMap[K] {
    const element = document.createElement(tag);
    if (text !== undefined) element.textContent = text;
    return element;
  },
  button(label: string, action: () => void): HTMLButtonElement {
    const button = document.createElement('button');
    button.type = 'button'; button.textContent = label; button.addEventListener('click', action);
    return button;
  },
});

// Official pages share the desktop's React, ReactDOM and UI kit binding.
const runtimeSlot = Symbol.for('mintcat.shared-ui-runtime.v1');
export interface SharedRuntime {
  React: typeof ReactRuntime;
  ReactDOMClient: typeof ReactDOMRuntime;
  flushSync: typeof FlushSync;
  createPortal: typeof CreatePortal;
  Antd: typeof AntdRuntime;
  Icons: typeof IconsRuntime;
  kit: typeof kit;
  openExternalUrl?: (url: string) => Promise<void>;
}
const runtimeRealm = globalThis as typeof globalThis & Record<symbol, Readonly<SharedRuntime> | undefined>;
export function bindSharedRuntime(runtime: SharedRuntime): void {
  if (runtimeRealm[runtimeSlot]) throw new Error('Shared UI runtime is already bound; restart the WebView to replace it');
  Object.defineProperty(runtimeRealm, runtimeSlot, { value: Object.freeze(runtime), writable: false, configurable: false });
}
export function getSharedRuntime(): Readonly<SharedRuntime> {
  const runtime = runtimeRealm[runtimeSlot];
  if (!runtime) throw new Error('No shared UI runtime is bound in this WebView');
  return runtime;
}
export async function openExternalUrl(rawUrl: string): Promise<void> {
  let parsed: URL;
  try { parsed = new URL(rawUrl); } catch { throw new Error('无效的外部链接地址'); }
  if (parsed.protocol !== 'https:' || parsed.username || parsed.password) {
    throw new Error('仅允许打开不含内嵌凭据的 HTTPS 外部链接');
  }
  const normalized = parsed.toString();
  const opener = runtimeRealm[runtimeSlot]?.openExternalUrl;
  if (opener) {
    await opener(normalized);
    return;
  }
  window.open(normalized, '_blank', 'noopener,noreferrer');
}
