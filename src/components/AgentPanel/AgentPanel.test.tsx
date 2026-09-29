/** @vitest-environment jsdom */
import React, {act} from 'react';
import {createRoot} from 'react-dom/client';
import {afterEach, describe, expect, it, vi} from 'vitest';
import type {AgentPageState} from './AgentViewModel';
class TestResizeObserver {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
}

vi.stubGlobal('ResizeObserver', TestResizeObserver);
Element.prototype.scrollIntoView = vi.fn();
const getComputedStyle = window.getComputedStyle.bind(window);
window.getComputedStyle = (element) => getComputedStyle(element);


const initialState: AgentPageState = {timeline: [], loading: false};
const fakeViewModel = {
    subscribe: vi.fn((listener: (state: AgentPageState) => void) => {
        listener(initialState);
        return () => undefined;
    }),
    send: vi.fn(async () => undefined),
    confirm: vi.fn(async () => undefined),
    reject: vi.fn(async () => undefined),
    clear: vi.fn(),
};

vi.mock('@/core/IoC', () => ({IoC: {get: vi.fn(async () => fakeViewModel)}}));
vi.mock('@tauri-apps/plugin-shell', () => ({open: vi.fn(async () => undefined)}));
vi.mock('i18next', () => ({t: (key: string) => key}));

import {AgentPanel} from './index';

describe('AgentPanel smoke', () => {
    let container: HTMLDivElement | undefined;

    afterEach(() => {
        container?.remove();
        container = undefined;
        vi.clearAllMocks();
    });

    it('renders the chat surface and submits a user message', async () => {
        container = document.createElement('div');
        document.body.appendChild(container);
        const root = createRoot(container);
        await act(async () => root.render(<AgentPanel open onClose={() => undefined} />));

        expect(document.body.textContent).toContain('agent.title');
        expect(document.body.textContent).toContain('agent.emptyHint');
        const input = document.body.querySelector('textarea');
        expect(input).not.toBeNull();

        await act(async () => {
            const valueSetter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set;
            valueSetter?.call(input, 'list disabled mods');
            input?.dispatchEvent(new Event('input', {bubbles: true}));
        });
        const sendButton = Array.from(document.body.querySelectorAll('button'))
            .find((button) => button.textContent?.includes('agent.send'));
        await act(async () => sendButton?.click());

        expect(fakeViewModel.send).toHaveBeenCalledWith('list disabled mods');
        await act(async () => root.unmount());
    });
});
