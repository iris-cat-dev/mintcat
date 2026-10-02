import { bindSharedRuntime, kit, QueryCache, SessionStore, validateUiManifest } from '@mintcat/sdk-ui';
import type { Disposable, UiContext } from '@mintcat/sdk-ui';
import * as React from 'react';
import * as ReactDOMClient from 'react-dom/client';
import { createPortal, flushSync } from 'react-dom';
import * as Antd from 'antd';
import * as Icons from '@ant-design/icons';

bindSharedRuntime({ React, ReactDOMClient, createPortal, flushSync, Antd, Icons, kit });

const root = document.querySelector<HTMLElement>('#app')!;
const status = document.querySelector<HTMLElement>('#status')!;
const controller = new AbortController();
const disposables: Disposable[] = [];
const cache = new QueryCache();
const session = new SessionStore(sessionStorage, 'mintcat.module-dev.session.v1');
let dirty = false;
addEventListener('beforeunload', event => {
  controller.abort();
  for (const disposable of disposables.reverse()) disposable.dispose();
  if (dirty) { event.preventDefault(); event.returnValue = ''; }
});
async function start(): Promise<void> {
const generation = await fetch('/generation').then(response => response.json());
const generationListeners = new Set<(generationId: string) => void | Promise<void>>();
const signed = await fetch('/manifest').then(response => response.json());
const descriptor = validateUiManifest(await fetch('/payload/ui/contributions.json').then(response => response.json()), signed.files.map((file: { path: string }) => file.path));
for (const path of descriptor.css) {
  const style = document.createElement('link'); style.rel = 'stylesheet'; style.href = `/payload/${path}`; document.head.append(style);
  disposables.push({ dispose() { style.remove(); } });
}
const request = async <T>(method: string, params: object, idempotencyKey?: string): Promise<T> => {
  if (idempotencyKey) throw new Error('This isolated worker host does not issue Application idempotency authority; use a real Application dev daemon for those domain requests.');
  const response = await fetch('/invoke', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ method, params }), signal: controller.signal });
  const value = await response.json(); if (!response.ok) throw new Error(value.error); return value;
};
const context: UiContext = {
  moduleId: descriptor.moduleId, signal: controller.signal, kit,
  request, call: request,
  own(disposable) { if (controller.signal.aborted) disposable.dispose(); else disposables.push(disposable); return disposable; },
  onGenerationChanged(listener) { generationListeners.add(listener); return context.own({ dispose() { generationListeners.delete(listener); } }); },
  query(key, load, ttl) { return cache.get(`${descriptor.moduleId}/${key}`, load, ttl); },
  invalidate(prefix) { cache.invalidate(`${descriptor.moduleId}/${prefix}`); },
  state(key, version, fallback) { return session.get(`${descriptor.moduleId}/${key}`, version, fallback); },
  saveState(key, version, value) { session.set(`${descriptor.moduleId}/${key}`, version, value); },
  dirty(value) { dirty = value; },
  resource(path) { if (!signed.files.some((file: { path: string }) => file.path === path)) throw new Error('Resource absent from signed generation'); return `/payload/${path}`; },
  notify(message) { status.textContent = message; },
  guard(action) { return (...args) => { Promise.resolve().then(() => action(...args)).catch(error => { status.textContent = String(error); }); }; },
  interval(action, milliseconds) { const timer = setInterval(context.guard(action), milliseconds); const disposable = { dispose() { clearInterval(timer); } }; disposables.push(disposable); return disposable; },
};
try {
  // The ESM specifier comes from this generation's authenticated module descriptor.
  const module = await import(`/payload/${descriptor.entry}`);
  if (module.activate) disposables.push(await module.activate(context));
  const selected = descriptor.pages.find(page => page.route === location.hash.slice(1)) ?? descriptor.pages[0];
  const navigation = kit.element('nav'); navigation.ariaLabel = '模块页面';
  for (const page of descriptor.pages) {
    const link = kit.element('a', page.title); link.href = `#${page.route}`;
    if (page === selected) link.ariaCurrent = 'page';
    link.addEventListener('click', event => { event.preventDefault(); if (page === selected) return; location.hash = page.route; location.reload(); });
    navigation.append(link);
  }
  if (descriptor.pages.length > 1) { root.before(navigation); disposables.push({ dispose: () => navigation.remove() }); }
  if (selected) disposables.push(await module[selected.export](root, context));
  const timer = setInterval(context.guard(async () => {
    const next = await fetch('/generation', { signal: controller.signal }).then(response => response.json());
    if (next.id === generation.id || controller.signal.aborted) return;
    if (dirty) { status.textContent = 'Source generation changed; save or discard the current draft before reloading.'; return; }
    await Promise.all([...generationListeners].map(listener => listener(next.id)));
    location.reload();
  }), 1000);
  disposables.push({ dispose() { clearInterval(timer); } });
} catch (error) { status.textContent = String(error); throw error; }
}
void start().catch(error => { status.textContent = String(error); console.error(error); });
