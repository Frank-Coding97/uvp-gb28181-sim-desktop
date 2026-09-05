export type SupportedLocale = "zh-CN" | "en-US";

export interface TranslationSchema {
  common: {
    confirm: string;
    cancel: string;
    save: string;
    delete: string;
    edit: string;
    status: string;
    actions: string;
    loading: string;
    success: string;
    failed: string;
    ready: string;
    running: string;
    stopped: string;
  };
  nav: {
    device: string;
    scenario: string;
    channels: string;
    dashboard: string;
    system: string;
  };
  scenario: {
    title: string;
    subtitle: string;
    targetPlatform: string;
    startStress: string;
    stopStress: string;
    exportReport: string;
    baseDeviceId: string;
    deviceCount: string;
    heartbeatInterval: string;
    channelsPerDevice: string;
    gbVersion: string;
    mediaProfile: string;
    bitrate: string;
    rampRate: string;
    activeRatio: string;
    positionReport: string;
    alarmReport: string;
  };
}
