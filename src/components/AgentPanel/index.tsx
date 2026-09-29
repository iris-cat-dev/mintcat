import React from 'react';
import ReactMarkdown from 'react-markdown';
import {Alert, Button, Card, Drawer, Empty, Flex, Input, Tag, Typography} from 'antd';
import {CheckOutlined, ClearOutlined, CloseOutlined, RobotOutlined, SendOutlined, ToolOutlined, UserOutlined} from '@ant-design/icons';
import {t} from 'i18next';
import {open} from '@tauri-apps/plugin-shell';
import {IoC} from '@/core/IoC';
import {AgentViewModel, type AgentPageState} from './AgentViewModel';
import './styles.css';

const {Text, Title} = Typography;
const {TextArea} = Input;

export interface AgentPanelProps {
    open: boolean;
    onClose: () => void;
}

export function AgentPanel({open: panelOpen, onClose}: AgentPanelProps) {
    const [state, setState] = React.useState<AgentPageState>({timeline: [], loading: false});
    const [input, setInput] = React.useState('');
    const viewModelRef = React.useRef<AgentViewModel | null>(null);
    const endRef = React.useRef<HTMLDivElement>(null);

    React.useEffect(() => {
        let active = true;
        let unsubscribe: (() => void) | undefined;
        void IoC.get(AgentViewModel).then((viewModel) => {
            if (!active) return;
            viewModelRef.current = viewModel;
            unsubscribe = viewModel.subscribe(setState);
        });
        return () => {
            active = false;
            unsubscribe?.();
        };
    }, []);

    React.useEffect(() => {
        endRef.current?.scrollIntoView({behavior: 'smooth'});
    }, [state.timeline, state.pendingAction, state.loading]);

    const send = async () => {
        const message = input.trim();
        if (!message || state.loading || state.pendingAction) return;
        setInput('');
        await viewModelRef.current?.send(message);
    };

    return (
        <Drawer
            title={(
                <div>
                    <div>{t('agent.title')}</div>
                    <Text type="secondary">{t('agent.subtitle')}</Text>
                </div>
            )}
            extra={(
                <Button icon={<ClearOutlined />} disabled={state.loading} onClick={() => viewModelRef.current?.clear()}>
                    {t('agent.newConversation')}
                </Button>
            )}
            placement="right"
            size={680}
            open={panelOpen}
            onClose={onClose}
            styles={{body: {padding: 0, overflow: 'hidden'}}}
        >
            <div className="agent-page">
            <div className="agent-message-list">
                {state.timeline.length === 0 ? (
                    <div className="agent-empty">
                        <Empty
                            image={<div className="agent-empty-icon-wrap"><RobotOutlined className="agent-empty-icon" /></div>}
                            description={<Text type="secondary">{t('agent.emptyHint')}</Text>}
                        />
                    </div>
                ) : state.timeline.map((item) => (
                    <div key={item.id} className={`agent-message agent-message-${item.role}`}>
                        <div className="agent-message-icon">
                            {item.role === 'user' ? <UserOutlined /> : item.role === 'assistant' ? <RobotOutlined /> : <ToolOutlined />}
                        </div>
                        <div className="agent-message-body">
                            {item.toolName ? <Tag className="agent-tool-tag">{item.toolName}</Tag> : null}
                            {item.role === 'assistant' ? (
                                <ReactMarkdown components={{
                                    a: ({href, children}) => <a onClick={() => href && void open(href)}>{children}</a>,
                                }}>{item.content}</ReactMarkdown>
                            ) : <Text type={item.role === 'error' ? 'danger' : undefined}>{item.content}</Text>}
                        </div>
                    </div>
                ))}

                {state.pendingAction ? (
                    <Card className="agent-confirmation" title={t('agent.confirmAction')}>
                        <Title level={5}>{state.pendingAction.title}</Title>
                        {state.pendingAction.details.map((detail) => <div key={detail}><Text>{detail}</Text></div>)}
                        <Flex gap={8} justify="flex-end" style={{marginTop: 16}}>
                            <Button icon={<CloseOutlined />} disabled={state.loading} onClick={() => void viewModelRef.current?.reject(state.pendingAction!.id)}>
                                {t('Cancel')}
                            </Button>
                            <Button
                                type="primary"
                                danger={state.pendingAction.request.name === 'remove_mod'}
                                icon={<CheckOutlined />}
                                loading={state.loading}
                                onClick={() => void viewModelRef.current?.confirm(state.pendingAction!.id)}
                            >
                                {t('Confirm')}
                            </Button>
                        </Flex>
                    </Card>
                ) : null}
                {state.error ? <Alert className="agent-error-alert" type="error" showIcon message={state.error} /> : null}
                {state.loading && !state.pendingAction ? (
                    <div className="agent-loading">
                        <span className="agent-typing"><span /><span /><span /></span>
                        <Text type="secondary">{t('agent.working')}</Text>
                    </div>
                ) : null}
                <div ref={endRef} />
            </div>

            <div className="agent-composer">
                <TextArea
                    value={input}
                    autoSize={{minRows: 2, maxRows: 5}}
                    disabled={state.loading || !!state.pendingAction}
                    placeholder={state.pendingAction ? t('agent.confirmFirst') : t('agent.inputPlaceholder')}
                    onChange={(event) => setInput(event.target.value)}
                    onPressEnter={(event) => {
                        if (!event.shiftKey) {
                            event.preventDefault();
                            void send();
                        }
                    }}
                />
                <Button className="agent-send-button" type="primary" icon={<SendOutlined />} disabled={!input.trim() || state.loading || !!state.pendingAction} onClick={() => void send()}>
                    {t('agent.send')}
                </Button>
            </div>
            </div>
        </Drawer>
    );
}
