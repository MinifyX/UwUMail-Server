import { ICONS, Icon, IconButton } from "@uwusuite/design";
import { Package } from "lucide-react";
import { seenFeaturesOf, showsNew, useMarkSeen } from "@/features/shell/newBadge";
import { useSession } from "@/features/session/session";
import { useT } from "@/i18n";
import { Link } from "@/lib/router";

/** The id in `seenFeatures` once an admin has closed the card, on any device. */
export const NEWS_ID = "news-spamnet-parcels";
const SINCE = "0.38.0";

/** Once, on the overview after an update: what is new about spam and parcels, and where to switch it on. */
export function NewsCard() {
  const { t } = useT();
  const session = useSession().data;
  const markSeen = useMarkSeen();
  if (!session) return null;
  const seen = seenFeaturesOf(session.preferences);
  if (!showsNew({ id: NEWS_ID, since: SINCE }, session.server.installedVersion, seen)) return null;
  return (
    <section
      aria-labelledby="news-title"
      className="flex flex-col gap-3 rounded-card border border-line bg-surface p-4"
    >
      <div className="flex items-start justify-between gap-3">
        <h2 id="news-title" className="text-sm font-bold text-ink">
          {t("news.spamnetParcels.title")}
        </h2>
        <IconButton
          icon={ICONS.close}
          size="sm"
          label={t("news.close")}
          onClick={() => markSeen.mutate([...seen, NEWS_ID].slice(-64))}
        />
      </div>
      <div className="grid gap-4 sm:grid-cols-2">
        {/* Spam-Netz: filled in by the Spam-Netz work package (A); it brings its own text and link. */}
        <div data-slot="spamnet" />
        <div className="flex items-start gap-3">
          <span className="flex size-9 shrink-0 items-center justify-center rounded-full bg-pink-tint text-pink-ink">
            <Icon icon={Package} className="size-[18px]" aria-hidden />
          </span>
          <div className="flex min-w-0 flex-col gap-1">
            <p className="text-sm font-semibold text-ink">{t("news.spamnetParcels.parcelsTitle")}</p>
            <p className="text-[13px] text-muted">{t("news.spamnetParcels.parcelsText")}</p>
            <Link
              to="/admin/settings/apps"
              className="inline-flex items-center gap-0.5 text-[13px] font-semibold text-pink-ink hover:underline"
            >
              {t("news.spamnetParcels.parcelsOpen")}
              <Icon icon={ICONS.next} className="size-3.5" aria-hidden />
            </Link>
          </div>
        </div>
      </div>
    </section>
  );
}
