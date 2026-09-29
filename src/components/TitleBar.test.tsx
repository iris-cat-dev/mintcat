/** @vitest-environment jsdom */
import React, {act} from 'react';
import {createRoot} from 'react-dom/client';
import {afterEach, describe, expect, it, vi} from 'vitest';
import type * as I18next from 'i18next';

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

vi.mock('i18next', async (importOriginal) => ({
    ...await importOriginal<typeof I18next>(),
    t: (key: string) => key,
}));
vi.mock('@/storage', () => ({
    StorageAPI: {
        getGames: vi.fn(async () => ({getActiveGame: vi.fn(async () => null)})),
    },
}));
vi.mock('@/events', () => ({
    emitEvent: vi.fn(async () => undefined),
    emitVoidEvent: vi.fn(async () => undefined),
    listenEvent: vi.fn(async () => () => undefined),
}));
vi.mock('../dialogs/UserSettingDialog/index.tsx', () => ({
    default: () => null,
}));

import TitleBar from './TitleBar';

describe('TitleBar agent action', () => {
    let container: HTMLDivElement | undefined;

    afterEach(() => {
        container?.remove();
        container = undefined;
        vi.clearAllMocks();
    });

    it('places the agent action with the header controls and opens it', async () => {
        const onAgentClick = vi.fn();
        container = document.createElement('div');
        document.body.appendChild(container);
        const root = createRoot(container);

        await act(async () => root.render(<TitleBar onAgentClick={onAgentClick}/>));

        const icon = container.querySelector('.anticon-robot');
        const button = icon?.closest('button');
        expect(button).not.toBeNull();
        expect(button?.parentElement?.tagName).toBe('SPAN');

        await act(async () => button?.click());
        expect(onAgentClick).toHaveBeenCalledOnce();

        await act(async () => root.unmount());
    });
});
