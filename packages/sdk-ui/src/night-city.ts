import type { ThemeConfig } from 'antd';

const ink = '#160e23';
const surface = '#261c32';
const selected = '#4a2d63';
const violet = '#cf9bff';
const yellow = '#f2f52e';

export const nightCityConfig: ThemeConfig = {
  cssVar: { key: 'mintcat-NightCity' },
  token: {
    colorPrimary: violet, colorInfo: violet, colorSuccess: '#6eddb2', colorWarning: '#ffb454', colorError: '#fa7387',
    colorText: '#f5effb', colorTextSecondary: '#cbbada', colorTextTertiary: '#ac9abb',
    colorBgLayout: 'transparent', colorBgContainer: surface, colorBgElevated: '#33243f',
    colorBorder: '#60506e', colorBorderSecondary: '#413149',
    borderRadius: 6, borderRadiusSM: 4, borderRadiusLG: 9,
    fontSize: 13, controlHeight: 36, controlHeightSM: 28,
    fontFamily: '-apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC", "Microsoft YaHei", sans-serif',
  },
  components: {
    Button: { colorPrimary: yellow, colorPrimaryHover: '#fdff75', colorPrimaryActive: '#dade20', primaryColor: ink, primaryShadow: 'none' },
    Menu: { itemBg: 'transparent', itemColor: '#cbbada', itemSelectedBg: selected, itemSelectedColor: violet, itemBorderRadius: 6 },
    Table: { colorBgContainer: 'transparent', headerBg: '#2c1d3a', headerColor: '#cbbada', headerSplitColor: 'transparent', rowHoverBg: '#392944', rowSelectedBg: selected, cellPaddingBlock: 8, cellPaddingInline: 12 },
    Card: { headerBg: 'transparent', paddingLG: 16 },
    Select: { optionSelectedBg: selected },
    Switch: { colorPrimary: '#52e5ed', handleBg: ink },
  },
};
