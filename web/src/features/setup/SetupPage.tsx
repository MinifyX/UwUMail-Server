import { ChevronRight, RefreshCw } from "lucide-react";
import { useEffect, useRef } from "react";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { useT } from "@/i18n";
import type { Session } from "@/lib/api";
import { Link } from "@/lib/router";
import { DnsStatusPill } from "@/features/domains/DnsBits";
import { useDomains } from "@/features/people/queries";
import { GatewayPanel, ReachabilityChecks } from "./GatewayBits";
import { AddressChecks, CheckedAt, Checking, DeliveryChecks, TestMailPanel } from "./SetupBits";
import { useLastReachability, useLastServerCheck, useRunReachability, useRunServerCheck } from "./queries";

/**
 * Server → Overview → Mail flow: how mail reaches this server and leaves it, checked again whenever
 * it is needed. The setup assistant itself is only for the very first start.
 */
export function MailFlowPage({ session }: { session: Session }) {
  const { t } = useT();
  const last = useLastServerCheck();
  const run = useRunServerCheck();
  const domains = useDomains();
  const reach = useLastReachability().data ?? null;
  const runReach = useRunReachability();
  const started = useRef(false);
  const check = last.data ?? null;

  useEffect(() => {
    if (last.isPending || last.data || started.current) return;
    started.current = true;
    run.mutate(false);
  }, [last.isPending, last.data, run]);

  const busyPlain = run.isPending && run.variables === false;

  return (
    <div className="flex flex-col gap-5">
      <p className="flex flex-wrap items-center gap-x-2 gap-y-1 rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">
        {t("setup.page.settingsHint")}
        <Link to="/admin/settings/mail" className="font-semibold text-pink-ink hover:underline">
          {t("setup.page.settingsLink")}
        </Link>
        <Link to="/admin/queue" className="font-semibold text-pink-ink hover:underline">
          {t("setup.page.queueLink")}
        </Link>
      </p>

      <div className="grid gap-5 lg:grid-cols-2">
        <Card
          title={t("setup.reach.cardTitle")}
          action={
            <Button size="sm" icon={RefreshCw} busy={runReach.isPending} onClick={() => runReach.mutate()}>
              {t(reach ? "setup.reach.run" : "setup.reach.start")}
            </Button>
          }
        >
          <div className="flex flex-col gap-4">
            {runReach.isPending && <Checking />}
            {reach && !runReach.isPending && (
              <>
                <CheckedAt check={reach} />
                <ReachabilityChecks reach={reach} explain={false} />
              </>
            )}
            {!reach && !runReach.isPending && <p className="text-[13px] text-muted">{t("setup.page.neverRun")}</p>}
          </div>
        </Card>
        <Card title={t("setup.gateway.title")}>
          <GatewayPanel hostname={session.server.hostname} explain={false} />
        </Card>
      </div>

      <Card
        title={t("setup.sending.title")}
        action={
          <Button size="sm" icon={RefreshCw} busy={busyPlain} onClick={() => run.mutate(false)}>
            {t("setup.sending.run")}
          </Button>
        }
      >
        <div className="flex flex-col gap-4">
          {busyPlain && <Checking />}
          {check && !busyPlain && (
            <>
              <CheckedAt check={check} />
              <DeliveryChecks check={check} explain={false} onRecheck={() => run.mutate(false)} />
            </>
          )}
          {!check && !run.isPending && <p className="text-[13px] text-muted">{t("setup.page.neverRun")}</p>}
        </div>
      </Card>

      <div className="grid gap-5 lg:grid-cols-2">
        <Card title={t("setup.checks.title")}>
          <div className="flex flex-col gap-4">
            {check && !busyPlain ? (
              <AddressChecks check={check} explain={false} busy={run.isPending} onBlocklists={() => run.mutate(true)} />
            ) : (
              busyPlain && <Checking />
            )}
          </div>
        </Card>

        <Card title={t("setup.testMail.title")}>
          <div className="flex flex-col gap-4">
            <TestMailPanel login={session.account.login} explain={false} />
          </div>
        </Card>
      </div>

      <Card title={t("setup.page.domainsTitle")}>
        <div className="flex flex-col gap-3">
          <ul className="flex flex-col">
            {(domains.data ?? []).map((domain) => (
              <li key={domain.name} className="border-b border-hairline last:border-b-0">
                <Link
                  to={`/admin/domains/${encodeURIComponent(domain.name)}`}
                  className="flex min-h-12 items-center gap-3 rounded-control px-1 py-1.5 hover:bg-pink-tint/40"
                >
                  <span className="min-w-0 flex-1 truncate text-sm font-semibold">{domain.name}</span>
                  <DnsStatusPill status={domain.dns?.status ?? null} />
                  <ChevronRight className="size-4 text-faint" aria-hidden />
                </Link>
              </li>
            ))}
          </ul>
        </div>
      </Card>
    </div>
  );
}
