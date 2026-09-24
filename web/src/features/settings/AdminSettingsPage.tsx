import { AppWindow, EarthLock, Palette, Send, SlidersHorizontal } from "lucide-react";
import { TabbedPage } from "@/components/ui/TabbedPage";
import { VpnPage } from "@/features/vpn/VpnPage";
import { useT } from "@/i18n";
import { BrandingPage } from "./BrandingPage";
import { SettingsPage, type SettingsTab } from "./SettingsPage";

/** Server → Settings: everything that holds for the whole server, VPN & proxy included. */
export type AdminSettingsTab = SettingsTab | "branding" | "vpn";
export const SETTINGS_PATHS: Record<AdminSettingsTab, string> = {
  general: "/admin/settings",
  mail: "/admin/settings/mail",
  apps: "/admin/settings/apps",
  branding: "/admin/settings/branding",
  vpn: "/admin/settings/vpn",
};
const ICONS = { general: SlidersHorizontal, mail: Send, apps: AppWindow, branding: Palette, vpn: EarthLock };
const INTROS: Record<AdminSettingsTab, string> = {
  general: "settings.intro",
  mail: "settings.mailIntro",
  apps: "settings.appsIntro",
  branding: "branding.intro",
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
      {tab === "vpn" ? <VpnPage /> : tab === "branding" ? <BrandingPage /> : <SettingsPage tab={tab} />}
    </TabbedPage>
  );
}
