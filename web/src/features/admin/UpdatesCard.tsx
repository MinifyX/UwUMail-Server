import { ArrowRight } from "lucide-react";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Loading } from "@/components/StatusViews";
import { useUpdates } from "@/features/updates/queries";
import { useT } from "@/i18n";
import { formatDateTime } from "@/lib/format";
import { navigate } from "@/lib/router";

/**
 * A line on the overview: which version is running and whether something is newer. Everything that
 * can be done about it lives on its own page, which this points at.
 */
export function UpdatesCard() {
  const { t, i18n } = useT();
  const query = useUpdates();

  if (query.isPending) {
    return (
      <Card title={t("updates.title")}>
        <Loading />
      </Card>
    );
  }
  // The overview should not turn red because GitHub was unreachable; the page says what went wrong.
  if (query.isError) return null;

  const view = query.data;
  const { build, info } = view;
  const edge = !build.release;
  const behind = info.behind ?? 0;
  const newest = info.releases[0];
  const available = edge ? behind > 0 : Boolean(newest);

  return (
    <Card
      title={t("updates.title")}
      action={
        <Button variant="ghost" size="sm" icon={ArrowRight} onClick={() => navigate("/admin/updates")}>
          {t("updates.open")}
        </Button>
      }
    >
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="min-w-0">
          <p className="text-sm">
            {t(edge ? "updates.runningEdge" : "updates.running", {
              version: build.version,
              commit: build.commit?.slice(0, 7) ?? "–",
            })}
          </p>
          <p className="text-[13px] text-muted">
            {available
              ? edge
                ? t("updates.edgeBehind", { count: behind })
                : t("updates.newer", { version: newest?.version })
              : info.checkedAt
                ? t("updates.upToDate", { time: formatDateTime(info.checkedAt, i18n.language) })
                : t("updates.notChecked")}
          </p>
        </div>
      </div>
    </Card>
  );
}
