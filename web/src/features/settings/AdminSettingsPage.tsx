import { ICONS } from "@uwusuite/design";
import { AppWindow, EarthLock, Palette } from "lucide-react";
import { TabbedPage } from "@/components/ui/TabbedPage";
import { AdminAssistPage } from "@/features/assist/AdminAssistPage";
import { PublicPicturesCard } from "@/features/pictures/PictureCard";
import { UwuauthPage } from "@/features/uwuauth/UwuauthPage";
import { VpnPage } from "@/features/vpn/VpnPage";
import { useT } from "@/i18n";
import { BrandingPage } from "./BrandingPage";
import { LoginSettingsPage } from "./LoginSettingsPage";
import { SettingsPage, type SettingsTab } from "./SettingsPage";

/** Server → Settings: everything that holds for the whole server, VPN & proxy included. */
export type AdminSettingsTab = SettingsTab | "login" | "uwuauth" | "branding" | "assist" | "vpn";
export const SETTINGS_PATHS: Record<AdminSettingsTab, string> = {
  general: "/admin/settings",
  mail: "/admin/settings/mail",
  apps: "/admin/settings/apps",
  login: "/admin/settings/login",
  uwuauth: "/admin/settings/uwuauth",
  branding: "/admin/settings/branding",
  assist: "/admin/settings/assist",
  vpn: "/admin/settings/vpn",
};
const KIND_ICONS = {
  general: ICONS.adjust,
  mail: ICONS.send,
  apps: AppWindow,
  login: ICONS.secret,
  uwuauth: ICONS.securityCheck,
  branding: Palette,
  assist: ICONS.ai,
  vpn: EarthLock,
};
const INTROS: Record<AdminSettingsTab, string> = {
  general: "settings.intro",
  mail: "settings.mailIntro",
  apps: "settings.appsIntro",
  login: "externalLogin.intro",
  uwuauth: "uwuauth.intro",
  branding: "branding.intro",
  assist: "assist.admin.intro",
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
        icon: KIND_ICONS[value],
        label: t(`settings.tabs.${value}`),
      }))}
    >
      {tab === "vpn" ? (
        <VpnPage />
      ) : tab === "assist" ? (
        <AdminAssistPage />
      ) : tab === "branding" ? (
        <BrandingPage />
      ) : tab === "uwuauth" ? (
        <UwuauthPage />
      ) : tab === "login" ? (
        <LoginSettingsPage />
      ) : tab === "general" ? (
        <div className="flex flex-col gap-5">
          <SettingsPage tab={tab} />
          <PublicPicturesCard />
        </div>
      ) : (
        <SettingsPage tab={tab} />
      )}
    </TabbedPage>
  );
}
