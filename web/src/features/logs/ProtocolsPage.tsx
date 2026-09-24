import { History, ScrollText, Send } from "lucide-react";
import { TabbedPage } from "@/components/ui/TabbedPage";
import { LogPage } from "@/features/log/LogPage";
import { useT } from "@/i18n";
import { LogsPage } from "./LogsPage";
import { LokiCard } from "./LokiCard";

/** Server → Logs: the live server log, who changed what, and where the log is sent. */
export type ProtocolsTab = "live" | "changes" | "shipping";
export const PROTOCOLS_PATHS: Record<ProtocolsTab, string> = {
  live: "/admin/logs",
  changes: "/admin/logs/changes",
  shipping: "/admin/logs/shipping",
};
const ICONS = { live: ScrollText, changes: History, shipping: Send };
const INTROS: Record<ProtocolsTab, string> = {
  live: "logs.intro",
  changes: "log.intro",
  shipping: "logs.shippingIntro",
};

export function ProtocolsPage({ tab = "live" }: { tab?: ProtocolsTab }) {
  const { t } = useT();
  return (
    <TabbedPage<ProtocolsTab>
      title={t("logs.title")}
      intro={t(INTROS[tab])}
      label={t("logs.tab")}
      value={tab}
      tabs={(Object.keys(PROTOCOLS_PATHS) as ProtocolsTab[]).map((value) => ({
        value,
        to: PROTOCOLS_PATHS[value],
        icon: ICONS[value],
        label: t(`logs.tabs.${value}`),
      }))}
    >
      {tab === "live" && <LogsPage />}
      {tab === "changes" && <LogPage />}
      {tab === "shipping" && <LokiCard />}
    </TabbedPage>
  );
}
