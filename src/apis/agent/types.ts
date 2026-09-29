export type AgentChatRole = 'system' | 'user' | 'assistant' | 'tool';

export interface AgentFunctionToolCall {
    id: string;
    type: 'function';
    function: {
        name: string;
        arguments: string;
    };
}

export interface AgentChatMessage {
    role: AgentChatRole;
    content: string | null;
    tool_calls?: AgentFunctionToolCall[];
    tool_call_id?: string;
}

export interface AgentFunctionDefinition {
    name: string;
    description: string;
    parameters: Record<string, unknown>;
}

export interface AgentFunctionTool {
    type: 'function';
    function: AgentFunctionDefinition;
}

export interface AgentChatCompletionRequest {
    model: string;
    messages: AgentChatMessage[];
    tools?: AgentFunctionTool[];
    tool_choice?: 'auto';
}

export interface AgentChatCompletionResponse {
    choices: Array<{
        finish_reason?: string | null;
        message: AgentChatMessage;
    }>;
}
