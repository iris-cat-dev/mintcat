/** @vitest-environment jsdom */
import React, {act} from 'react';
import {createRoot} from 'react-dom/client';
import {afterEach, describe, expect, it, vi} from 'vitest';

const {settings, setApiKey, createChatCompletion} = vi.hoisted(() => ({
    settings: {
        getAgentLlmBaseUrl: vi.fn(async () => 'https://llm.example/v1'),
        getAgentLlmModel: vi.fn(async () => 'test-model'),
        setAgentLlmBaseUrl: vi.fn(async () => undefined),
        setAgentLlmModel: vi.fn(async () => undefined),
    },
    setApiKey: vi.fn(async () => undefined),
    createChatCompletion: vi.fn(async () => ({
        choices: [{message: {role: 'assistant', content: 'OK'}}],
    })),
}));

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
window.matchMedia = vi.fn().mockImplementation((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: vi.fn(),
    removeListener: vi.fn(),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    dispatchEvent: vi.fn(),
}));

vi.mock('i18next', () => ({t: (key: string) => key}));
vi.mock('@/storage', () => ({StorageAPI: {getSettings: vi.fn(async () => settings)}}));
vi.mock('@/apis/agent/OpenAICompatibleApi', () => ({
    OpenAICompatibleApi: {
        hasApiKey: vi.fn(async () => false),
        setApiKey,
        createChatCompletion,
    },
}));

import {AgentSettings} from './AgentSettings';

describe('AgentSettings connection test', () => {
    let container: HTMLDivElement | undefined;

    afterEach(() => {
        container?.remove();
        container = undefined;
        vi.clearAllMocks();
    });

    it('saves the current settings before testing the connection', async () => {
        container = document.createElement('div');
        document.body.appendChild(container);
        const root = createRoot(container);
        await act(async () => root.render(<AgentSettings/>));

        const apiKeyInput = container.querySelector<HTMLInputElement>('input[type="password"]');
        await act(async () => {
            const valueSetter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set;
            valueSetter?.call(apiKeyInput, 'test-api-key');
            apiKeyInput?.dispatchEvent(new Event('input', {bubbles: true}));
        });

        const testButton = Array.from(container.querySelectorAll('button'))
            .find((button) => button.textContent?.includes('Test Connection'));
        await act(async () => testButton?.click());

        expect(settings.setAgentLlmBaseUrl).toHaveBeenCalledWith('https://llm.example/v1');
        expect(settings.setAgentLlmModel).toHaveBeenCalledWith('test-model');
        expect(setApiKey).toHaveBeenCalledWith('test-api-key');
        expect(createChatCompletion).toHaveBeenCalledWith(
            'https://llm.example/v1',
            expect.objectContaining({model: 'test-model'}),
            'test-api-key',
        );
        expect(setApiKey.mock.invocationCallOrder[0]).toBeLessThan(
            createChatCompletion.mock.invocationCallOrder[0],
        );
        await act(async () => root.unmount());
    });
});
