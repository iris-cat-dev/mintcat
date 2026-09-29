import {BaseViewModel} from '@/core/BaseViewModel';
import {AgentService, type AgentTimelineItem} from '@/services/AgentService';
import type {PreparedAgentAction} from '@/services/ModAgentToolService';
import {captureNexusDownloadUrl} from '@/apis/nexusmods/downloadWebview';

export interface AgentPageState {
    timeline: AgentTimelineItem[];
    pendingAction?: PreparedAgentAction;
    loading: boolean;
    error?: string;
}

export class AgentViewModel extends BaseViewModel {
    private readonly agentService = new AgentService();
    private state: AgentPageState = {timeline: [], loading: false};
    private readonly listeners = new Set<(state: AgentPageState) => void>();

    public override async initialize(): Promise<void> {
        this.initialized = true;
    }

    public subscribe(listener: (state: AgentPageState) => void): () => void {
        this.listeners.add(listener);
        listener(this.snapshot());
        return () => this.listeners.delete(listener);
    }

    public async send(content: string): Promise<void> {
        if (this.state.loading) return;
        this.setState({loading: true, error: undefined});
        try {
            const result = await this.agentService.sendUserMessage(content);
            this.setState({timeline: this.agentService.getTimeline(), pendingAction: result.pendingAction, loading: false});
        } catch (error) {
            this.setState({timeline: this.agentService.getTimeline(), loading: false, error: error instanceof Error ? error.message : String(error)});
        }
    }

    public async confirm(actionId: string): Promise<void> {
        if (this.state.loading) return;
        this.setState({loading: true, error: undefined});
        try {
            const result = await this.agentService.confirmPendingAction(
                actionId,
                (url, parsed) => captureNexusDownloadUrl(url, {parsed}),
            );
            this.setState({timeline: this.agentService.getTimeline(), pendingAction: result.pendingAction, loading: false});
        } catch (error) {
            this.setState({
                timeline: this.agentService.getTimeline(),
                pendingAction: this.agentService.getPendingAction(),
                loading: false,
                error: error instanceof Error ? error.message : String(error),
            });
        }
    }

    public async reject(actionId: string): Promise<void> {
        if (this.state.loading) return;
        this.setState({loading: true, error: undefined});
        try {
            const result = await this.agentService.rejectPendingAction(actionId);
            this.setState({timeline: this.agentService.getTimeline(), pendingAction: result.pendingAction, loading: false});
        } catch (error) {
            this.setState({
                timeline: this.agentService.getTimeline(),
                pendingAction: this.agentService.getPendingAction(),
                loading: false,
                error: error instanceof Error ? error.message : String(error),
            });
        }
    }

    public clear(): void {
        this.agentService.clearConversation();
        this.setState({timeline: [], pendingAction: undefined, loading: false, error: undefined});
    }

    private snapshot(): AgentPageState {
        return {
            ...this.state,
            timeline: this.state.timeline.map((item) => ({...item})),
            pendingAction: this.state.pendingAction
                ? {...this.state.pendingAction, details: [...this.state.pendingAction.details]}
                : undefined,
        };
    }

    private setState(next: Partial<AgentPageState>): void {
        this.state = {...this.state, ...next};
        const snapshot = this.snapshot();
        this.listeners.forEach((listener) => listener(snapshot));
    }
}
