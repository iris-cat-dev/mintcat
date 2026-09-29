import React from 'react';
import {Alert, Button, Card, Form, Input, Space, Typography, message} from 'antd';
import {t} from 'i18next';
import {StorageAPI} from '@/storage';
import {OpenAICompatibleApi} from '@/apis/agent/OpenAICompatibleApi';
import {SettingLayout} from '@/pages/SettingPage/Layout';

const {Text} = Typography;

export function AgentSettings() {
    const [baseUrl, setBaseUrl] = React.useState('');
    const [model, setModel] = React.useState('');
    const [apiKey, setApiKey] = React.useState('');
    const [hasApiKey, setHasApiKey] = React.useState(false);
    const [saving, setSaving] = React.useState(false);
    const [testing, setTesting] = React.useState(false);

    const loadSettings = React.useCallback(async () => {
        const settings = await StorageAPI.getSettings();
        const [storedBaseUrl, storedModel, configured] = await Promise.all([
            settings.getAgentLlmBaseUrl(),
            settings.getAgentLlmModel(),
            OpenAICompatibleApi.hasApiKey(),
        ]);
        setBaseUrl(storedBaseUrl);
        setModel(storedModel);
        setHasApiKey(configured);
    }, []);

    React.useEffect(() => {
        void loadSettings().catch((error) => {
            console.error('[AgentSettings] Failed to load settings:', error);
            message.error(t('agent.settingsLoadFailed'));
        });
    }, [loadSettings]);

    const saveSettings = async (showSuccess = true): Promise<boolean> => {
        const normalizedUrl = baseUrl.trim();
        const normalizedModel = model.trim();
        if (!normalizedUrl || !normalizedModel) {
            message.error(t('agent.settingsRequired'));
            return false;
        }

        setSaving(true);
        try {
            const settings = await StorageAPI.getSettings();
            await Promise.all([
                settings.setAgentLlmBaseUrl(normalizedUrl),
                settings.setAgentLlmModel(normalizedModel),
                apiKey.trim() ? OpenAICompatibleApi.setApiKey(apiKey) : Promise.resolve(),
            ]);
            if (apiKey.trim()) {
                setHasApiKey(true);
                setApiKey('');
            }
            if (showSuccess) {
                message.success(t('agent.settingsSaved'));
            }
            return true;
        } catch (error) {
            console.error('[AgentSettings] Failed to save settings:', error);
            message.error(t('agent.settingsSaveFailed'));
            return false;
        } finally {
            setSaving(false);
        }
    };

    const clearApiKey = async () => {
        try {
            await OpenAICompatibleApi.clearApiKey();
            setApiKey('');
            setHasApiKey(false);
            message.success(t('agent.apiKeyCleared'));
        } catch (error) {
            console.error('[AgentSettings] Failed to clear API key:', error);
            message.error(t('agent.settingsSaveFailed'));
        }
    };

    const testConnection = async () => {
        const enteredApiKey = apiKey.trim();
        setTesting(true);
        try {
            if (!await saveSettings(false)) {
                return;
            }
            await OpenAICompatibleApi.createChatCompletion(baseUrl.trim(), {
                model: model.trim(),
                messages: [
                    {role: 'system', content: 'Reply with OK.'},
                    {role: 'user', content: 'Connection test'},
                ],
            }, enteredApiKey);
            message.success(t('agent.connectionSucceeded'));
        } catch (error) {
            console.error('[AgentSettings] Connection test failed:', error);
            message.error(error instanceof Error ? error.message : String(error));
        } finally {
            setTesting(false);
        }
    };

    const insecureTransport = baseUrl.trim().toLowerCase().startsWith('http://');

    return (
        <Card title={t('agent.settingsTitle')} style={{marginBottom: '10px'}}>
            <Form {...SettingLayout}>
                <Form.Item label={t('agent.baseUrl')} required>
                    <Input value={baseUrl} onChange={(event) => setBaseUrl(event.target.value)} placeholder="https://api.openai.com/v1" />
                </Form.Item>
                <Form.Item label={t('agent.model')} required>
                    <Input value={model} onChange={(event) => setModel(event.target.value)} placeholder="gpt-4.1-mini" />
                </Form.Item>
                <Form.Item label={t('agent.apiKey')}>
                    <Space orientation="vertical" style={{width: '100%'}}>
                        <Input.Password
                            value={apiKey}
                            onChange={(event) => setApiKey(event.target.value)}
                            placeholder={hasApiKey ? t('agent.apiKeyConfigured') : t('agent.apiKeyOptional')}
                        />
                        <Text type="secondary">{t('agent.apiKeyStoredSecurely')}</Text>
                        {hasApiKey ? <Button danger onClick={() => void clearApiKey()}>{t('agent.clearApiKey')}</Button> : null}
                    </Space>
                </Form.Item>
                {insecureTransport ? <Alert type="warning" showIcon message={t('agent.insecureTransportWarning')} style={{marginBottom: 16}} /> : null}
                <Form.Item>
                    <Space>
                        <Button type="primary" loading={saving} onClick={() => void saveSettings()}>{t('Save')}</Button>
                        <Button loading={testing} onClick={() => void testConnection()}>{t('Test Connection')}</Button>
                    </Space>
                </Form.Item>
            </Form>
        </Card>
    );
}
