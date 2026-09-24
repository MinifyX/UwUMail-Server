import { AppWindow, EarthLock, Send, SlidersHorizontal } from "lucide-react";
import { TabbedPage } from "@/components/ui/TabbedPage";
import { VpnPage } from "@/features/vpn/VpnPage";
import { useT } from "@/i18n";
import { SettingsPage, type SettingsTab } from "./SettingsPage";

/** Server → Settings: everything that holds for the whole server, VPN & proxy included. */
export type AdminSettingsTab = SettingsTab | "vpn";
export const SETTINGS_PATHS: Record<AdminSettingsTab, string> = {
  general: "/admin/settings",
  mail: "/admin/settings/mail",
  apps: "/admin/settings/apps",
  vpn: "/admin/settings/vpn",
};
const ICONS = { general: SlidersHorizontal, mail: Send, apps: AppWindow, vpn: EarthLock };
const INTROS: Record<AdminSettingsTab, string> = {
  general: "settings.intro",
  mail: "settings.mailIntro",
  apps: "settings.appsIntro",
  vpn: "vpn.intro",
};

export function AdminSettingsPage({ tab = "general" }: { tab?: AdminSettingsTab }) {
  const { t } = useT();
  return (
    <TabbedPage<AdminSettingsTab>
      title={t("settings.title")}
      intro={t(INTROS[tab])}
      label={t("settings.tab")}
      value={tab}
      tabs={(Object.keys(SETTINGS_PATHS) as AdminSettingsTab[]).map((value) => ({
        value,
        to: SETTINGS_PATHS[value],
        icon: ICONS[value],
        label: t(`settings.tabs.${value}`),
      }))}
    >
      {tab === "vpn" ? <VpnPage /> : <SettingsPage tab={tab} />}
    </TabbedPage>
  );
}
