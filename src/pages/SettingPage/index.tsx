import {BasePage} from "../IBasePage.ts";
import {MintCatSettings} from "@/pages/SettingPage/MintCatSettings.tsx";
import {NetworkSettings} from "@/pages/SettingPage/NetworkSettings.tsx";
import {CloudBackupSettings} from "@/pages/SettingPage/CloudBackupSettings.tsx";
import {AgentSettings} from "@/pages/SettingPage/AgentSettings.tsx";


export class SettingPage extends BasePage<any, any> {

    public constructor(props: any) {
        super(props);
    }

    componentDidMount(): void {
        this.hookWindowResized();
    }

    render() {
        return (
            <div
                id="scrollableDiv"
                className="scrollable-div"
                style={{ height: window.innerHeight - 81 }}>
                <MintCatSettings/>
                <NetworkSettings/>
                <AgentSettings/>
                <CloudBackupSettings/>
            </div>
        );
    }
}
