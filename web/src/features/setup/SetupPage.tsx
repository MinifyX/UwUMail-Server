import { ChevronRight, RefreshCw, WandSparkles } from "lucide-react";
import { useEffect, useRef } from "react";
import { NyuScene } from "@/components/nyu/scenes";
import { Button } from "@/components/ui/Button";
import { Card, PageHeader } from "@/components/ui/Card";
import { useT } from "@/i18n";
import type { Session } from "@/lib/api";
import { Link, navigate } from "@/lib/router";
import { usePrefs } from "@/state/prefs";
import { DnsStatusPill } from "@/features/domains/DnsBits";
import { useDomains } from "@/features/people/queries";
import { GatewayPanel, ReachabilityChecks } from "./GatewayBits";
import { AddressChecks, CheckedAt, Checking, DeliveryChecks, TestMailPanel } from "./SetupBits";
import { useLastReachability, useLastServerCheck, useRunReachability, useRunServerCheck } from "./queries";

/** Server → Setup: the checks of the setup assistant, whenever they are needed again. */
export function SetupPage({ session }: { session: Session }) {
  const { t } = useT();
  const explain = usePrefs((s) => s.mode) === "simple";
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
      <PageHeader
        title={t("setup.page.title")}
        intro={t("setup.page.intro")}
        art={<NyuScene name="search" className="h-auto w-[150px]" />}
      />
      <div className="flex flex-wrap gap-2">
        <Button icon={WandSparkles} onClick={() => navigate("/setup")}>
          {t("setup.page.wizard")}
        </Button>
      </div>

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
            {explain && <p className="-mt-1 text-[13px] text-muted">{t("setup.reach.body")}</p>}
            {runReach.isPending && <Checking />}
            {reach && !runReach.isPending && (
              <>
                <CheckedAt check={reach} />
                <ReachabilityChecks reach={reach} explain={explain} />
              </>
            )}
          </div>
        </Card>
        <Card title={t("setup.gateway.title")}>
          <GatewayPanel hostname={session.server.hostname} explain={explain} />
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
          {explain && <p className="-mt-1 text-[13px] text-muted">{t("setup.sending.body")}</p>}
          {busyPlain && <Checking />}
          {check && !busyPlain && (
            <>
              <CheckedAt check={check} />
              <DeliveryChecks check={check} explain={explain} onRecheck={() => run.mutate(false)} />
            </>
          )}
          {!check && !run.isPending && <p className="text-[13px] text-muted">{t("setup.page.neverRun")}</p>}
        </div>
      </Card>

      <div className="grid gap-5 lg:grid-cols-2">
        <Card title={t("setup.checks.title")}>
          <div className="flex flex-col gap-4">
            {explain && <p className="-mt-1 text-[13px] text-muted">{t("setup.checks.body")}</p>}
            {check && !busyPlain ? (
              <AddressChecks
                check={check}
                explain={explain}
                busy={run.isPending}
                onBlocklists={() => run.mutate(true)}
              />
            ) : (
              busyPlain && <Checking />
            )}
          </div>
        </Card>

        <Card title={t("setup.testMail.title")}>
          <div className="flex flex-col gap-4">
            {explain && <p className="-mt-1 text-[13px] text-muted">{t("setup.testMail.body")}</p>}
            <TestMailPanel login={session.account.login} explain={explain} />
          </div>
        </Card>
      </div>

      <Card title={t("setup.page.domainsTitle")}>
        <div className="flex flex-col gap-3">
          {explain && <p className="-mt-1 text-[13px] text-muted">{t("setup.page.domainsBody")}</p>}
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
