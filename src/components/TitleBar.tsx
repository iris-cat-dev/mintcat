import React from "react";
import {Avatar, Badge, Button, Flex, Image, Popover, Space, Tooltip, message} from "antd";
import {t} from "i18next";
import {
    BellOutlined,
    BookOutlined,
    CloudOutlined,
    EllipsisOutlined,
    PlayCircleOutlined,
    RobotOutlined,
    SkinOutlined,
    SyncOutlined,
    UserOutlined
} from '@ant-design/icons';
import {open} from "@tauri-apps/plugin-shell";
import packageJson from '../../package.json';
import {IntegrateApi} from "../apis/IntegrateApi.ts";
import {StorageAPI} from "@/storage";
import {emitEvent, emitVoidEvent, listenEvent, UnlistenFn} from "@/events";
import { taskQueueAPI } from "tauri-plugin-task-queue";
import UserSettingDialog from "../dialogs/UserSettingDialog/index.tsx";
import { CloudBackupApi } from "@/apis/mintcat";
import StatusBar from "./StatusBar.tsx";


interface TitleBarProps {
    onAgentClick: () => void;
}

class TitleBar extends React.Component<TitleBarProps, any> {

    private readonly userSettingDialogRef: React.RefObject<UserSettingDialog>
    private unlistenActiveGameChange: UnlistenFn | undefined;
    private unlistenConfigImported: UnlistenFn | undefined;
    private unlistenUserSettingOpen: UnlistenFn | undefined;
    private unlistenModioUnauthorized: UnlistenFn | undefined;
    private unlistenInstallFailedGamePath: UnlistenFn | undefined;

    public constructor(props: any) {
        super(props);

        this.userSettingDialogRef = React.createRef();

        this.state = {
            gameName: "未选择",
            avatarUrl: null,
            cloudBackupLoading: false,
            guidePopoverOpen: false,
        };

        this.onLaunchGameClick = this.onLaunchGameClick.bind(this);
        this.onCloudBackupClick = this.onCloudBackupClick.bind(this);
    }

    private getDisplayVersion() {
        if (packageJson.channel === "stable") {
            return `v${packageJson.version}`;
        }

        return `v${packageJson.version}-${packageJson.channel}-${packageJson["sub-version"]}`;
    }

    private async loadActiveGame() {
        try {
            const gameDAO = await StorageAPI.getGames();
            const activeGame = await gameDAO.getActiveGame();
            if (activeGame) {
                this.setState({ gameName: activeGame.displayName });
            }
        } catch (e) {
            console.error("Failed to load active game", e);
        }
    }

    private readonly HELP_DOC_URL = "https://www.mintcat.work";

    private async onOpenHelpDoc() {
        this.setState({ guidePopoverOpen: false });
        await open(this.HELP_DOC_URL);
    }

    private async onShowOnboarding() {
        this.setState({ guidePopoverOpen: false });
        await emitVoidEvent("start-onboarding");
    }

    private async onLaunchGameClick() {
        try {
            // 检查当前 profile 是否有 mod，没有则直接启动游戏
            const profilesApi = await StorageAPI.getProfiles();
            const activeProfile = await profilesApi.getActiveProfile();
            if (!activeProfile?.id) {
                await IntegrateApi.launchGame();
                return;
            }
            const profileMods = await profilesApi.getProfileMods(activeProfile.id);
            if (profileMods.length === 0) {
                await IntegrateApi.launchGame();
                return;
            }

            // Submit installation task
            const taskId = await IntegrateApi.installMods();

            // Wait for task to complete (no timeout)
            const result = await taskQueueAPI.waitForTaskCompletion(taskId);

            if (result.status === 'completed') {
                // Installation succeeded, notify HomePage and launch game
                await emitVoidEvent('mods-installed');
                await IntegrateApi.launchGame();
            } else if (result.status === 'failed') {
                const baseError = t("Installation Failed");
                const detail = result.error || 'Unknown error';
                const msg = detail.startsWith(baseError) ? detail : `${baseError}: ${detail}`;
                if (detail.includes(t('Game Path Not Found'))) {
                    await emitEvent('install-failed-game-path-not-found', msg);
                } else {
                    message.error(msg);
                }
            }
        } catch (error) {
            console.error('[TitleBar] Installation failed:', error);
            message.error(t("Installation Failed"));
        }
    }

    private async onCloudBackupClick() {
        // 检查是否已配置云备份
        try {
            const config = await CloudBackupApi.getConfig();
            if (!config.baseUrl) {
                message.warning(t("cloudBackup.configureFirst"));
                return;
            }

            // 开始旋转动画
            this.setState({ cloudBackupLoading: true });
            
            // 在状态栏输出开始提示
            await StatusBar.info(t("Cloud backup syncing..."));

            // 执行云备份
            await CloudBackupApi.createBackup();

            // 在状态栏输出完成提示
            await StatusBar.success(t("Cloud backup completed"));
            message.success(t("Cloud backup completed"));
        } catch (error) {
            console.error("[TitleBar] Cloud backup failed:", error);
            await StatusBar.error(t("Cloud backup failed") + `: ${error}`);
            message.error(t("Cloud backup failed"));
        } finally {
            // 停止旋转动画
            this.setState({ cloudBackupLoading: false });
        }
    }

    private async onThemeClick(value: string) {
        const storage = await StorageAPI.getSettings();
        await storage.setGuiTheme(value);
        localStorage.setItem('theme', value);
        await emitEvent("theme-change", value as 'Light' | 'Dark' | 'Pink');
    }

    async componentDidMount() {
        this.loadActiveGame();
        this.unlistenActiveGameChange = await listenEvent('active-game-change', (game) => {
            this.setState({ gameName: game.displayName });
        });
        this.unlistenConfigImported = await listenEvent('config-imported', () => {
            this.loadActiveGame();
        });
        this.unlistenUserSettingOpen = await listenEvent('user-setting-dialog-open', () => {
            this.userSettingDialogRef.current?.show();
        });
        this.unlistenModioUnauthorized = await listenEvent('modio-unauthorized', (errorMessage: string) => {
            const key = 'modio-unauthorized';
            message.error({
                key,
                content: (
                    <span>
                        {errorMessage}
                        {' '}
                        <Button type="link" size="small" style={{ padding: 0, height: 'auto' }} onClick={() => {
                            message.destroy(key);
                            this.userSettingDialogRef.current?.show();
                        }}>
                            {t("Open User Management")}
                        </Button>
                    </span>
                ),
                duration: 6,
            });
        });
        this.unlistenInstallFailedGamePath = await listenEvent('install-failed-game-path-not-found', (errorMessage: string) => {
            const key = 'install-failed-game-path';
            message.error({
                key,
                content: (
                    <span>
                        {errorMessage}
                        {' '}
                        <Button type="link" size="small" style={{ padding: 0, height: 'auto' }} onClick={() => {
                            message.destroy(key);
                            emitVoidEvent('select-game-dialog-open');
                        }}>
                            {t("Select Game")}
                        </Button>
                    </span>
                ),
                duration: 6,
            });
        });
    }

    componentWillUnmount() {
        if (this.unlistenActiveGameChange) {
            this.unlistenActiveGameChange();
        }
        if (this.unlistenConfigImported) {
            this.unlistenConfigImported();
        }
        if (this.unlistenUserSettingOpen) {
            this.unlistenUserSettingOpen();
        }
        if (this.unlistenModioUnauthorized) {
            this.unlistenModioUnauthorized();
        }
        if (this.unlistenInstallFailedGamePath) {
            this.unlistenInstallFailedGamePath();
        }
    }

    render() {
        return (
            <Flex justify={"space-between"}>
                <Flex gap="middle" wrap>
                    <Image
                        width={30}
                        preview={false}
                        src="icon.ico"
                    />
                    <h1>
                        <b>
                            <span style={{
                                backgroundImage: "linear-gradient(to right, blue, purple)",
                                backgroundClip: "text",
                                color: "transparent",
                            }}>MINT</span>
                            <span style={{
                                backgroundImage: "linear-gradient(to right, purple, deeppink)",
                                backgroundClip: "text",
                                color: "transparent",
                            }}>CAT</span>
                        </b>
                    </h1>
                    <span style={{fontSize: "12px", color: "gray", lineHeight: "54px", verticalAlign: "bottom"}}>
                        {this.getDisplayVersion()}
                    </span>
                </Flex>
                <Flex gap="small" justify={"flex-end"} wrap>
                    <span>
                        <Space.Compact block>
                            <Button type="primary"
                                onClick={this.onLaunchGameClick}
                                className={"ant-header-start-button tour-step-launch"}
                            >
                                <PlayCircleOutlined/>
                                <span style={{marginTop: "-1px"}}>
                                    <b>
                                        {this.state.gameName}
                                    </b>
                                </span>
                            </Button>
                            <Tooltip title={t("Select Game")}>
                                <Button type="primary" 
                                    className={"ant-header-start-button tour-step-switch-game"}
                                    icon={<EllipsisOutlined />} 
                                    onClick={() => emitVoidEvent('select-game-dialog-open')}
                                />
                            </Tooltip>
                        </Space.Compact>
                    </span>
                    <span>
                        <Tooltip title={t("Cloud Backup")}>
                            <Button type={"text"}
                                    icon={this.state.cloudBackupLoading ? <SyncOutlined spin /> : <CloudOutlined />}
                                    onClick={this.onCloudBackupClick}
                                    disabled={this.state.cloudBackupLoading}
                            />
                        </Tooltip>
                    </span>
                    <span>
                        <Badge size={"small"}
                               count={0}
                        >
                         <Button type={"text"}
                                 icon={<BellOutlined/>}
                         />
                        </Badge>
                    </span>
                    <span>
                    <Popover
                        placement="bottom"
                        title={""}
                        content={
                            <div style={{display: 'grid', gridTemplateColumns: 'repeat(2, 1fr)', gap: 16}}>
                                {[
                                    {key: 'Light', title: t('Light'), color: "#804bcc"},
                                    {key: 'Blue', title: t('Blue'), color: "#F5F8FF"},
                                    {key: 'Dark', title: t('Dark'), color: "black"},
                                    {key: 'Pink', title: t('Pink'), color: "rgba(237,65,146,0.2)"},
                                ].map((item) => (
                                    <Button
                                        key={item.key}
                                        className={"app-title-bar-skin-button"}
                                        title={item.title}
                                        style={{backgroundColor: item.color}}
                                        onClick={async () => {
                                            await this.onThemeClick(item.key);
                                        }}
                                    />
                                ))}
                            </div>
                        }
                    >
                       <Button type={"text"}
                               icon={<SkinOutlined/>}
                       />
                    </Popover>
                    </span>
                    <span>
                        <Popover
                            placement="bottomRight"
                            trigger="click"
                            open={this.state.guidePopoverOpen}
                            onOpenChange={(open) => this.setState({ guidePopoverOpen: open })}
                            title={t("Beginner's Guide")}
                            content={
                                <Flex vertical gap={8}>
                                    <Button type="text" icon={<BookOutlined/>} block style={{ justifyContent: 'flex-start' }} onClick={() => this.onShowOnboarding()}>
                                        {t("Show onboarding again")}
                                    </Button>
                                    <Button type="text" icon={<BookOutlined/>} block style={{ justifyContent: 'flex-start' }} onClick={() => this.onOpenHelpDoc()}>
                                        {t("Help Document")}
                                    </Button>
                                </Flex>
                            }
                        >
                            <Tooltip title={t("Beginner's Guide")}>
                                <Button type={"text"} icon={<BookOutlined/>}/>
                            </Tooltip>
                        </Popover>
                    </span>
                    <span>
                        <Tooltip title={t("agent.title")}>
                            <Button type={"text"} icon={<RobotOutlined/>} onClick={this.props.onAgentClick}/>
                        </Tooltip>
                    </span>
                    <Avatar className={"app-header-avatar tour-step-avatar"}
                            icon={<UserOutlined/>}
                            src={this.state.avatarUrl}
                            onClick={() => {
                                this.userSettingDialogRef.current?.show();
                            }}
                    />
                </Flex>
                <UserSettingDialog ref={this.userSettingDialogRef}/>
            </Flex>
        );
    }
}

export default TitleBar;
