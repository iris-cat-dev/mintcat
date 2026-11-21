import type { ThemeConfig } from 'antd';

const ink = '#0a101a';
const surface = '#151f2c';
const selected = '#193742';
const cyan = '#52e5ed';
const yellow = '#f2f52e';

export const nightCityConfig: ThemeConfig = {
  cssVar: { key: 'mintcat-NightCity' },
  token: {
    colorPrimary: cyan, colorInfo: cyan, colorSuccess: '#6eddb2', colorWarning: '#ffb454', colorError: '#fa7387',
    colorText: '#edf5fc', colorTextSecondary: '#b0bdcd', colorTextTertiary: '#92a5bb',
    colorBgLayout: 'transparent', colorBgContainer: surface, colorBgElevated: '#1b2938',
    colorBorder: '#435568', colorBorderSecondary: '#2b394a',
    borderRadius: 6, borderRadiusSM: 4, borderRadiusLG: 9,
    fontSize: 13, controlHeight: 36, controlHeightSM: 28,
    fontFamily: '-apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC", "Microsoft YaHei", sans-serif',
  },
  components: {
    Button: { colorPrimary: yellow, colorPrimaryHover: '#fdff75', colorPrimaryActive: '#dade20', primaryColor: ink, primaryShadow: 'none' },
    Menu: { itemBg: 'transparent', itemColor: '#b0bdcd', itemSelectedBg: selected, itemSelectedColor: cyan, itemBorderRadius: 6 },
    Table: { colorBgContainer: 'transparent', headerBg: '#14222e', headerColor: '#b0bdcd', headerSplitColor: 'transparent', rowHoverBg: '#192c39', rowSelectedBg: selected, cellPaddingBlock: 8, cellPaddingInline: 12 },
    Card: { headerBg: 'transparent', paddingLG: 16 },
    Select: { optionSelectedBg: selected },
    Switch: { handleBg: ink },
  },
};
