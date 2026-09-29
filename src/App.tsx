import React from 'react';
import {Layout} from 'antd';
import {Route, Routes, useLocation, useNavigate} from 'react-router-dom';
import {useAppError} from '@/hooks/useAppError';
import {useDeepLinkHandler} from '@/hooks/useDeepLinkHandler';
import {useOAuthCallback} from '@/hooks/useOAuthCallback';

import TitleBar from "@/components/TitleBar.tsx";
import StatusBar from "@/components/StatusBar.tsx";
import MenuBar, {MenuPage} from "@/components/MenuBar.tsx";
import UpdateDialog from "@/dialogs/UpdateDialog.tsx";
import {initClipboardWatcher} from "@/dialogs/AddModDialog/open.ts";

import {HomePage} from "@/pages/HomePage";
import {SearchPage} from "@/pages/SearchPage";
import {SettingPage} from "@/pages/SettingPage";
import {AcceleratorPage} from "@/pages/AcceleratorPage";
import {AgentPanel} from "@/components/AgentPanel";
import {AppInitializer} from "@/core/AppInitializer";

import './App.css';
import {EmptyPage} from "@/pages/EmptyPage.tsx";
import {ConfigManageDialog} from "@/dialogs/ConfigManageDialog";

import {SelectGameDialog} from "@/dialogs/SelectGameDialog";
import {useKeyboardListener} from "@/hooks/useKeyboardListener.tsx";
import {emitEvent, listenEvent, useEventListener} from "@/events";
import {OnboardingTour} from "@/components/OnboardingTour.tsx";
import {DeviceApi} from "@/apis/DeviceApi.ts";
import {IoC} from "@/core/IoC";
import {AppViewModel} from "@/AppViewModel";

const {
    Header,
    Footer,
    Sider,
    Content
} = Layout;


const AppContent = () => {


    const [isAppViewModelReady, setIsAppViewModelReady] = React.useState(false);
    const [initError, setInitError] = React.useState<Error | null>(null);
    const [showOnboarding, setShowOnboarding] = React.useState(false);
    const [agentPanelOpen, setAgentPanelOpen] = React.useState(false);
    const onboardingOpenedByUserRef = React.useRef(false);
    const navigate = useNavigate();
    const location = useLocation();

    const clickMenu = React.useCallback((key: string) => {
        if (key === MenuPage.Home) {
            navigate('/home');
            return;
        }
        navigate(`/home/${key}`);
    }, [navigate]);

    const activeMenuKey = React.useMemo<MenuPage>(() => {
        const trimmed = location.pathname.replace(/^\/home\/?/, '');
        const segment = trimmed.split('/')[0];
        if (!segment) {
            return MenuPage.Home;
        }
        if (Object.values(MenuPage).includes(segment as MenuPage)) {
            return segment as MenuPage;
        }
        return MenuPage.Home;
    }, [location.pathname]);

    useAppError();
    useDeepLinkHandler(isAppViewModelReady);
    useOAuthCallback();
    // 配置导入成功后刷新主题和语言，使界面立即反映迁移后的设置
    useEventListener('config-imported', async () => {
        try {
            const vm = await IoC.get(AppViewModel);
            await vm.loadUserGuiTheme();
            await vm.loadUserLanguages();
        } catch (e) {
            console.warn('[App] config-imported refresh theme/language failed', e);
        }
    });
    useKeyboardListener((event) => {
        if (event.ctrlKey && event.key === 'f') {
            event.preventDefault();
        }
    });

    React.useEffect(() => {
        console.log('App 组件加载...');

        // Initialize core (database + AppViewModel)
        setInitError(null);
        AppInitializer.initializeCore()
            .then(async () => {
                console.log('[App] Core initialization complete');
                setInitError(null);
                setIsAppViewModelReady(true);
                const completed = await DeviceApi.getOnboardingCompleted();
                if (!completed) {
                    // 首次启动：触发新手指引（欢迎 → 配置导入 → 游戏选择 → 用户设置 → 其余步骤）
                    requestAnimationFrame(() => {
                        setTimeout(() => setShowOnboarding(true), 100);
                    });
                }
            })
            .catch((error) => {
                console.error('[App] Core initialization failed:', error);
                setInitError(error instanceof Error ? error : new Error(String(error)));
                emitEvent('app-error', error.message || 'error.applicationInitFailed').catch(console.error);
            });

        initClipboardWatcher();

    }, []);

    React.useEffect(() => {
        let unlisten: (() => void) | undefined;
        listenEvent('start-onboarding', () => {
            onboardingOpenedByUserRef.current = true;
            requestAnimationFrame(() => setTimeout(() => setShowOnboarding(true), 100));
        }).then((fn) => { unlisten = fn; });
        return () => { unlisten?.(); };
    }, []);

    const handleOnboardingComplete = React.useCallback(async () => {
        setShowOnboarding(false);
        if (!onboardingOpenedByUserRef.current) {
            await DeviceApi.setOnboardingCompleted();
        }
        onboardingOpenedByUserRef.current = false;
    }, []);

    return (
        <Layout className={"app"}>
            <OnboardingTour open={showOnboarding} onComplete={handleOnboardingComplete}/>
            <UpdateDialog/>
            <ConfigManageDialog/>
            <SelectGameDialog/>
            {isAppViewModelReady ? <AgentPanel open={agentPanelOpen} onClose={() => setAgentPanelOpen(false)}/> : null}
            <Header className={"app-header"}>
                <TitleBar onAgentClick={() => setAgentPanelOpen(true)}/>
            </Header>
            <Layout>
                <Sider width="50px">
                    <MenuBar onClick={clickMenu} activeKey={activeMenuKey}/>
                </Sider>
                <Content>
                    {initError != null && (
                        <EmptyPage initError={initError} onRetry={() => {
                            setInitError(null);
                            AppInitializer.resetForRetry();
                            AppInitializer.initializeCore()
                                .then(async () => {
                                    setInitError(null);
                                    setIsAppViewModelReady(true);
                                    const completed = await DeviceApi.getOnboardingCompleted();
                                    if (!completed) {
                                        requestAnimationFrame(() => {
                                            setTimeout(() => setShowOnboarding(true), 100);
                                        });
                                    }
                                })
                                .catch((err) => {
                                    setInitError(err instanceof Error ? err : new Error(String(err)));
                                    emitEvent('app-error', err.message || 'error.applicationInitFailed').catch(console.error);
                                });
                        }}/>
                    )}
                    {initError == null && !isAppViewModelReady && <EmptyPage/>}
                    {initError == null && isAppViewModelReady && (
                        <Routes>
                            <Route path="/home" element={<HomePage/>}/>
                            <Route path="/home/modio" element={<SearchPage/>}/>
                            <Route path="/home/accelerator" element={<AcceleratorPage/>}/>
                            <Route path="/home/setting" element={<SettingPage/>}/>
                            <Route path="*" element={<HomePage/>}/>
                        </Routes>
                    )}
                </Content>
            </Layout>
            <Footer className="app-footer" style={{height: "30px", position: "relative", zIndex: 5}}>
                <StatusBar/>
            </Footer>
        </Layout>
    );
};

function App() {
    return (
        <AppContent/>
    );
}


export default App;
