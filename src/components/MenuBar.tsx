import React from "react";
import {Menu, MenuProps} from "antd";
import {HomeOutlined, SettingOutlined} from "@ant-design/icons";
import {ModioOutlined} from "./SvgIcon.tsx";
import {t} from "i18next";


type MenuItem = Required<MenuProps>['items'][number];

export enum MenuPage {
    Home = 'home',
    Setting = 'setting',
    Modio = 'modio',
    // Chat = 'chat',
    // Accelerator = 'accelerator',
}

interface MenuBarProps {
    onClick: (key: string) => void;
    activeKey: string;
}

class MenuBar extends React.Component<MenuBarProps, any> {

    private items: MenuItem[] = [
        {
            key: MenuPage.Home,
            icon: <HomeOutlined/>,
            label: t("Home"),
            className: 'tour-step-home',
        },
        {key: MenuPage.Modio, icon: <ModioOutlined/>, label: 'mod.io', className: 'tour-step-modio'},
        // {key: MenuPage.Accelerator, icon: <ThunderboltOutlined/>, label: t('Accelerator')},
        {key: MenuPage.Setting, icon: <SettingOutlined/>, label: t("Settings"), className: 'tour-step-setting'},
    ];

    public constructor(props: any) {
        super(props);
    }

    render() {
        return (
            <Menu
                selectedKeys={[this.props.activeKey]}
                mode="inline"
                inlineIndent={14}
                items={this.items}
                style={{backgroundColor: 'transparent', height: '100%'}}
                onClick={({key}) => {
                    this.props.onClick(key);
                }}
            />
        );
    }
}

export default MenuBar;
