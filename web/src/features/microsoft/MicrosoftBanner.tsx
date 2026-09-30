import clsx from "clsx";
import { ChevronRight, MailWarning } from "lucide-react";
import { useT } from "@/i18n";
import { Link } from "@/lib/router";
import { bannerFacts, worstKind } from "./microsoft";
import { DELIST_URL, ExternalAnchor, useMicrosoftIssues } from "./MicrosoftPage";

/** On the server overview while Microsoft refuses or slows down mail from this server. */
export function MicrosoftBanner() {
  const { t } = useT();
  const query = useMicrosoftIssues();
  const issues = query.data?.issues ?? [];
  const kind = worstKind(issues);
  if (!kind) return null;
  const { codes, ips, delist } = bannerFacts(issues);
  const throttled = kind === "throttled";
  return (
    <section
      role="alert"
      className={clsx(
        "flex items-start gap-3 rounded-card border p-4",
        throttled ? "border-warning/30 bg-warning-tint" : "border-danger/30 bg-danger-tint",
      )}
    >
      <span
        className={clsx(
          "flex size-9 shrink-0 items-center justify-center rounded-full bg-surface",
          throttled ? "text-warning" : "text-danger",
        )}
      >
        <MailWarning className="size-[18px]" aria-hidden />
      </span>
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <p className="text-sm font-bold text-ink">{t(`microsoft.banner.${kind}`)}</p>
        <p className="text-[13px] text-muted">
          {t("microsoft.banner.codes", { codes: codes.join(", ") })}
          {ips.length > 0 && ` · ${t("microsoft.banner.ips", { ips: ips.join(", ") })}`}
        </p>
        <div className="mt-1 flex flex-wrap gap-x-4 gap-y-1 text-[13px]">
          {delist && (
            <ExternalAnchor href={query.data?.delistUrl || DELIST_URL}>{t("microsoft.banner.delist")}</ExternalAnchor>
          )}
          <Link
            to="/admin/microsoft"
            className="inline-flex items-center gap-0.5 font-semibold text-pink-ink hover:underline"
          >
            {t("microsoft.banner.open")}
            <ChevronRight className="size-3.5" aria-hidden />
          </Link>
        </div>
      </div>
    </section>
  );
}
