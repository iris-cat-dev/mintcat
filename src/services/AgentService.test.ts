import {describe, expect, it, vi} from 'vitest';
import {AgentService, type AgentConfigurationProvider, type AgentToolExecutor, type CompletionClient} from './AgentService';
import {parseAgentToolRequest} from './AgentToolRegistry';
import type {AgentChatCompletionResponse} from '@/apis/agent/types';

function mutationResponse(): AgentChatCompletionResponse {
    return {
        choices: [{
            message: {
                role: 'assistant',
                content: null,
                tool_calls: [{
                    id: 'call-remove',
                    type: 'function',
                    function: {name: 'remove_mod', arguments: '{"modId":42}'},
                }],
            },
        }],
    };
}

function finalResponse(content: string): AgentChatCompletionResponse {
    return {choices: [{message: {role: 'assistant', content}}]};
}

function createHarness() {
    const responses = [mutationResponse(), finalResponse('Completed')];
    const completionClient: CompletionClient = {
        create: vi.fn(async () => {
            const response = responses.shift();
            if (!response) throw new Error('Unexpected model call');
            return response;
        }),
    };
    const toolService: AgentToolExecutor = {
        executeRead: vi.fn(async () => ({ok: true})),
        prepareMutation: vi.fn(async (request) => ({
            id: 'pending-1',
            profileId: 7,
            request,
            title: 'Remove Example',
            details: ['Profile: Test'],
            createdAt: Date.now(),
        })),
        executeMutation: vi.fn(async () => ({ok: true, data: {removedFromProfile: true}})),
    };
    const configurationProvider: AgentConfigurationProvider = {
        load: vi.fn(async () => ({baseUrl: 'http://localhost:1234/v1', model: 'test-model'})),
    };
    return {service: new AgentService(toolService, completionClient, configurationProvider), completionClient, toolService};
}

describe('Agent tool argument validation', () => {
    it('normalizes safe defaults', () => {
        expect(parseAgentToolRequest('list_mods', '{}')).toEqual({name: 'list_mods', args: {limit: 50}});
        expect(parseAgentToolRequest('move_mod', '{"modId":1,"folderId":0}')).toEqual({name: 'move_mod', args: {modId: 1, folderId: 0}});
    });

    it('rejects malformed, unknown, and out-of-range arguments', () => {
        expect(() => parseAgentToolRequest('remove_mod', '{')).toThrow('valid JSON');
        expect(() => parseAgentToolRequest('remove_mod', '{"modId":1,"force":true}')).toThrow('Unknown tool argument');
        expect(() => parseAgentToolRequest('list_mods', '{"limit":101}')).toThrow('limit must be an integer');
        expect(() => parseAgentToolRequest('unknown', '{}')).toThrow('Unknown tool');
    });
});

describe('Agent mutation confirmation', () => {
    it('does not execute a mutation before explicit confirmation', async () => {
        const {service, completionClient, toolService} = createHarness();
        const result = await service.sendUserMessage('Remove Example');

        expect(result.pendingAction?.id).toBe('pending-1');
        expect(toolService.executeMutation).not.toHaveBeenCalled();
        expect(completionClient.create).toHaveBeenCalledTimes(1);

        await service.confirmPendingAction('pending-1', async (url) => url);
        expect(toolService.executeMutation).toHaveBeenCalledTimes(1);
        expect(completionClient.create).toHaveBeenCalledTimes(2);
        expect(service.getTimeline().at(-1)?.content).toBe('Completed');
    });

    it('reports rejection to the model without executing the mutation', async () => {
        const {service, toolService} = createHarness();
        await service.sendUserMessage('Remove Example');
        await service.rejectPendingAction('pending-1');

        expect(toolService.executeMutation).not.toHaveBeenCalled();
        expect(service.getTimeline().some((item) => item.content === 'Action rejected')).toBe(true);
        expect(service.getTimeline().at(-1)?.content).toBe('Completed');
    });

    it('rejects a mismatched confirmation identifier', async () => {
        const {service, toolService} = createHarness();
        await service.sendUserMessage('Remove Example');

        await expect(service.confirmPendingAction('wrong-id', async (url) => url)).rejects.toThrow('missing or expired');
        expect(toolService.executeMutation).not.toHaveBeenCalled();
    });
});
