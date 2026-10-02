import { Globe, Inbox, Plus, Truck } from "lucide-react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { EmptyState } from "@/components/ui/EmptyState";
import { useT } from "@/i18n";
import type { MoveInfo } from "@/lib/api";
import { formatDateTime, formatNumber } from "@/lib/format";
import { Link, navigate } from "@/lib/router";
import { MoveStatePill, ProgressBar } from "./MoveBits";
import { moveUrl, newMovePath, share } from "./moves";
import { useMoves } from "./queries";

function MoveRowCard({ move }: { move: MoveInfo }) {
  const { t, i18n } = useT();
  const { summary } = move;
  const Icon = move.kind === "domain" ? Globe : Inbox;
  return (
    <Link
      to={moveUrl(move.id)}
      className="flex flex-col gap-2 rounded-card border border-hairline bg-surface p-4 transition-colors hover:border-faint/60"
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="flex min-w-0 items-center gap-2 font-semibold">
          <Icon className="size-4 shrink-0 text-muted" aria-hidden />
          <span className="truncate">{move.domain}</span>
          <span className="text-[12px] font-normal text-muted">
            {t(`moves.kind.${move.kind}`)} · {t("moves.list.from", { host: move.imapHost })}
          </span>
        </span>
        <MoveStatePill state={move.state} />
      </div>
      <ProgressBar
        value={share(summary.messagesDone, summary.messagesTotal)}
        label={t("moves.progress.label", { domain: move.domain })}
      />
      <p className="text-[12px] text-muted">
        {t("moves.list.summary", {
          mailboxes: formatNumber(summary.mailboxes, i18n.language),
          done: formatNumber(summary.messagesDone, i18n.language),
          total: formatNumber(summary.messagesTotal, i18n.language),
        })}
        {summary.paused > 0 && ` · ${t("moves.list.paused", { count: summary.paused })}`}
        {" · "}
        {t("moves.list.created", { date: formatDateTime(move.createdAt, i18n.language) })}
      </p>
    </Link>
  );
}

/** Server → Accounts & domains → Moves: every move the admin started, newest first. */
export function MovesPage() {
  const { t } = useT();
  const moves = useMoves();
  if (moves.isPending) return <Loading />;
  if (moves.isError) return <LoadError error={moves.error} onRetry={() => void moves.refetch()} />;
  const list = moves.data.moves;
  return (
    <div className="flex flex-col gap-4">
      <div className="flex justify-end">
        <Button variant="primary" icon={Plus} onClick={() => navigate(newMovePath)}>
          {t("moves.list.new")}
        </Button>
      </div>
      {list.length === 0 ? (
        <Card>
          <EmptyState
            scene="inbox"
            title={t("moves.list.emptyTitle")}
            body={t("moves.list.emptyBody")}
            action={
              <Button icon={Truck} onClick={() => navigate(newMovePath)}>
                {t("moves.list.new")}
              </Button>
            }
          />
        </Card>
      ) : (
        list.map((move) => <MoveRowCard key={move.id} move={move} />)
      )}
      <p className="text-[12px] text-muted">{t("moves.list.own")}</p>
    </div>
  );
}
