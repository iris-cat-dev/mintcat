import {ControlPlaneApi} from '@/apis/ControlPlaneApi';
import type {AgentChatCompletionRequest, AgentChatCompletionResponse} from './types';

function isChatCompletionResponse(value: unknown): value is AgentChatCompletionResponse {
    if (!value || typeof value !== 'object' || !('choices' in value)) return false;
    const choices = value.choices;
    if (!Array.isArray(choices) || choices.length === 0) return false;
    const firstChoice = choices[0];
    if (!firstChoice || typeof firstChoice !== 'object' || !('message' in firstChoice)) return false;
    const message = firstChoice.message;
    return !!message
        && typeof message === 'object'
        && 'role' in message
        && message.role === 'assistant';
}

export class OpenAICompatibleApi {
    public static async setApiKey(apiKey: string): Promise<void> {
        await ControlPlaneApi.invoke('agent_llm_set_api_key', {apiKey});
    }

    public static async clearApiKey(): Promise<void> {
        await ControlPlaneApi.invoke('agent_llm_clear_api_key');
    }

    public static async hasApiKey(): Promise<boolean> {
        const result = await ControlPlaneApi.invoke<{configured: boolean}>('agent_llm_has_api_key');
        return result.configured === true;
    }

    public static async createChatCompletion(
        baseUrl: string,
        request: AgentChatCompletionRequest,
        apiKey?: string,
    ): Promise<AgentChatCompletionResponse> {
        const response = await ControlPlaneApi.invoke<unknown>('agent_llm_chat_completion', {
            baseUrl,
            body: request,
            ...(apiKey?.trim() ? {apiKey: apiKey.trim()} : {}),
        });
        if (!isChatCompletionResponse(response)) {
            throw new Error('The model returned an invalid Chat Completions response');
        }
        return response;
    }
}
