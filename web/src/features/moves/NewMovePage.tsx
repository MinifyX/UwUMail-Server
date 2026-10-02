import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, CheckCircle2, Search, Truck } from "lucide-react";
import { useMemo, useState, type ReactNode } from "react";
import { Button } from "@/components/ui/Button";
import { Card, PageHeader } from "@/components/ui/Card";
import { Field, Segmented, Select, TextInput, Toggle } from "@/components/ui/Field";
import { useMailDomains, usePeople } from "@/features/people/queries";
import { useT } from "@/i18n";
import {
  ApiError,
  api,
  type DavMode,
  type MoveDetail,
  type MoveDiscovery,
  type MoveKind,
  type MovePlan,
  type MoveRowProblem,
} from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { Link, navigate } from "@/lib/router";
import { toast } from "@/state/toasts";
import { emptyRow, moveUrl, movesPath, problemsByRow, rowsToBody, tableIndexes, type EditRow } from "./moves";
import { movesKey } from "./queries";
import { RowsEditor } from "./RowsEditor";

export const DAV_MODES: DavMode[] = ["auto", "sogo", "nextcloud", "icloud", "gmx", "webde", "custom", "none"];

function Step({ number, title, children }: { number: number; title: string; children: ReactNode }) {
  return (
    <Card
      title={
        <span className="flex items-center gap-2">
          <span className="inline-flex size-6 items-center justify-center rounded-full bg-pink-tint text-[12px] font-bold text-pink-ink">
            {number}
          </span>
          {title}
        </span>
      }
    >
      <div className="flex flex-col gap-4">{children}</div>
    </Card>
  );
}

/** Server → Moves → New: the wizard for a domain move or a single mailbox. */
export function NewMovePage() {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const domains = useMailDomains();
  const people = usePeople();
  const [kind, setKind] = useState<MoveKind>("domain");
  const [domain, setDomain] = useState("");
  const [imapHost, setImapHost] = useState("");
  const [imapPort, setImapPort] = useState("993");
  const [davMode, setDavMode] = useState<DavMode>("auto");
  const [davHost, setDavHost] = useState("");
  const [davUrl, setDavUrl] = useState("");
  const [contacts, setContacts] = useState(true);
  const [calendars, setCalendars] = useState(true);
  const [parallel, setParallel] = useState("2");
  const [syncMinutes, setSyncMinutes] = useState("60");
  const [rows, setRows] = useState<EditRow[]>([emptyRow()]);
  // A single move: into a mailbox that is there, or a new one.
  const [target, setTarget] = useState<"existing" | "new">("existing");
  const [existing, setExisting] = useState("");
  const [newLocal, setNewLocal] = useState("");
  const [plan, setPlan] = useState<MovePlan | null>(null);

  const cleanDomain = domain.trim().toLowerCase();
  const knownDomain = (domains.data ?? []).some((known) => known.name === cleanDomain);
  const mailboxesOnDomain = useMemo(
    () =>
      (people.data ?? []).filter(
        (person) => person.hasMailbox && person.status !== "deleted" && person.login.endsWith(`@${cleanDomain}`),
      ),
    [people.data, cleanDomain],
  );
  const singleTarget =
    target === "existing" ? existing : newLocal.trim() ? `${newLocal.trim().toLowerCase()}@${cleanDomain}` : "";
  const effectiveRows = kind === "mailbox" ? rows.slice(0, 1).map((row) => ({ ...row, target: singleTarget })) : rows;

  const body = (dryRun: boolean) => ({
    kind,
    domain: cleanDomain,
    imapHost: imapHost.trim(),
    imapPort: Number(imapPort) || 993,
    davMode,
    davHost: davHost.trim(),
    davUrl: davUrl.trim(),
    contacts,
    calendars,
    parallel: Number(parallel) || 2,
    syncMinutes: Number(syncMinutes) || 60,
    rows: rowsToBody(effectiveRows),
    dryRun,
  });

  const discover = useMutation({
    mutationFn: () =>
      api<MoveDiscovery>("/api/admin/moves/discover", {
        method: "POST",
        body: { address: rows.find((row) => row.oldAddress.includes("@"))?.oldAddress || cleanDomain },
      }),
    onSuccess: (found) => {
      if (found.imap) {
        setImapHost(found.imap.host);
        setImapPort(String(found.imap.port));
      }
      setDavMode(found.davMode);
      toast(
        found.imap ? t("moves.wizard.found", { host: found.imap.host }) : t("moves.wizard.notFound"),
        found.imap ? "success" : "error",
      );
    },
    onError: (error) => toast(errorText(error), "error"),
  });

  const check = useMutation({
    mutationFn: () => api<MovePlan>("/api/admin/moves", { method: "POST", body: body(true) }),
    onSuccess: (result) => setPlan(result),
  });

  const start = useMutation({
    mutationFn: () => api<MoveDetail>("/api/admin/moves", { method: "POST", body: body(false) }),
    onSuccess: (detail) => {
      void queryClient.invalidateQueries({ queryKey: movesKey, exact: true });
      void queryClient.invalidateQueries({ queryKey: ["admin", "people"] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "domains"] });
      toast(t("moves.toasts.started"), "success");
      navigate(moveUrl(detail.move.id));
    },
    onError: (error) => {
      if (error instanceof ApiError && error.code === "moveRows") void check.mutateAsync();
    },
  });

  // The check speaks of the rows it was sent; the table shows them where they are.
  const problems = useMemo(() => {
    const byTable = new Map<number, MoveRowProblem[]>();
    if (!plan) return byTable;
    const indexes = tableIndexes(effectiveRows);
    for (const [row, list] of problemsByRow(plan.problems)) byTable.set(indexes[row] ?? row, list);
    return byTable;
  }, [plan, effectiveRows]);

  const changed =
    <T,>(set: (value: T) => void) =>
    (value: T) => {
      set(value);
      setPlan(null);
    };
  const ready = plan !== null && plan.problems.length === 0;
  const sourceMissing = !imapHost.trim() || !cleanDomain;

  return (
    <div className="flex flex-col gap-5">
      <Link
        to={movesPath}
        className="inline-flex items-center gap-1.5 self-start text-[13px] font-semibold text-pink-ink"
      >
        <ArrowLeft className="size-4" aria-hidden />
        {t("moves.wizard.back")}
      </Link>
      <PageHeader title={t("moves.wizard.title")} intro={t("moves.wizard.intro")} />

      <Step number={1} title={t("moves.wizard.whatTitle")}>
        <Segmented<MoveKind>
          label={t("moves.wizard.whatTitle")}
          value={kind}
          onChange={changed(setKind)}
          options={[
            { value: "domain", label: t("moves.kind.domain") },
            { value: "mailbox", label: t("moves.kind.mailbox") },
          ]}
        />
        <p className="text-[13px] text-muted">{t(`moves.wizard.what.${kind}`)}</p>
        <Field
          label={t("moves.wizard.domain")}
          hint={
            cleanDomain && !knownDomain
              ? kind === "domain"
                ? t("moves.wizard.domainNew")
                : t("moves.wizard.domainUnknown")
              : t("moves.wizard.domainHint")
          }
        >
          {(id) => (
            <>
              <TextInput
                id={id}
                list="move-domains"
                autoCapitalize="none"
                spellCheck={false}
                placeholder="example.com"
                value={domain}
                onChange={(event) => changed(setDomain)(event.target.value.replace(/\s/g, ""))}
              />
              <datalist id="move-domains">
                {(domains.data ?? []).map((known) => (
                  <option key={known.name} value={known.name} />
                ))}
              </datalist>
            </>
          )}
        </Field>
        {kind === "mailbox" && knownDomain && (
          <div className="flex flex-col gap-3">
            <Segmented<"existing" | "new">
              label={t("moves.wizard.targetLabel")}
              value={target}
              onChange={changed(setTarget)}
              options={[
                { value: "existing", label: t("moves.wizard.targetExisting") },
                { value: "new", label: t("moves.wizard.targetNew") },
              ]}
            />
            {target === "existing" ? (
              <Field label={t("moves.wizard.mailbox")} hint={t("moves.wizard.mailboxHint")}>
                {(id) => (
                  <Select id={id} value={existing} onChange={(event) => changed(setExisting)(event.target.value)}>
                    <option value="">{t("moves.wizard.choose")}</option>
                    {mailboxesOnDomain.map((person) => (
                      <option key={person.login} value={person.login}>
                        {person.name ? `${person.name} <${person.login}>` : person.login}
                      </option>
                    ))}
                  </Select>
                )}
              </Field>
            ) : (
              <Field label={t("moves.wizard.newAddress")} hint={t("moves.wizard.newAddressHint")}>
                {(id) => (
                  <div className="flex items-center gap-2">
                    <TextInput
                      id={id}
                      autoCapitalize="none"
                      spellCheck={false}
                      value={newLocal}
                      onChange={(event) => changed(setNewLocal)(event.target.value.replace(/[\s@]/g, ""))}
                    />
                    <span className="text-sm text-muted">@{cleanDomain}</span>
                  </div>
                )}
              </Field>
            )}
          </div>
        )}
      </Step>

      <Step number={2} title={t("moves.wizard.sourceTitle")}>
        <p className="text-[13px] text-muted">{t("moves.wizard.sourceExplain")}</p>
        <div className="grid gap-3 sm:grid-cols-[1fr_110px_auto] sm:items-end">
          <Field label={t("moves.wizard.imapHost")} hint={t("moves.wizard.imapHostHint")}>
            {(id) => (
              <TextInput
                id={id}
                autoCapitalize="none"
                spellCheck={false}
                placeholder="imap.example.com"
                value={imapHost}
                onChange={(event) => changed(setImapHost)(event.target.value.trim())}
              />
            )}
          </Field>
          <Field label={t("moves.wizard.port")}>
            {(id) => (
              <TextInput
                id={id}
                type="number"
                min={1}
                max={65535}
                value={imapPort}
                onChange={(event) => changed(setImapPort)(event.target.value)}
              />
            )}
          </Field>
          <Button
            icon={Search}
            className="sm:mb-[22px]"
            busy={discover.isPending}
            disabled={!cleanDomain}
            onClick={() => discover.mutate()}
          >
            {t("moves.wizard.discover")}
          </Button>
        </div>
        <div className="flex flex-col gap-3 rounded-control border border-hairline p-3">
          <Toggle checked={contacts} onChange={changed(setContacts)} label={t("moves.wizard.contacts")} />
          <Toggle checked={calendars} onChange={changed(setCalendars)} label={t("moves.wizard.calendars")} />
          {(contacts || calendars) && (
            <div className="grid gap-3 sm:grid-cols-2">
              <Field label={t("moves.wizard.davMode")} hint={t(`moves.davModes.${davMode}Hint`)}>
                {(id) => (
                  <Select
                    id={id}
                    value={davMode}
                    onChange={(event) => changed(setDavMode)(event.target.value as DavMode)}
                  >
                    {DAV_MODES.map((mode) => (
                      <option key={mode} value={mode}>
                        {t(`moves.davModes.${mode}`)}
                      </option>
                    ))}
                  </Select>
                )}
              </Field>
              {(davMode === "sogo" || davMode === "nextcloud") && (
                <Field label={t("moves.wizard.davHost")} hint={t("moves.wizard.davHostHint")}>
                  {(id) => (
                    <TextInput
                      id={id}
                      autoCapitalize="none"
                      spellCheck={false}
                      placeholder={imapHost || "dav.example.com"}
                      value={davHost}
                      onChange={(event) => changed(setDavHost)(event.target.value.trim())}
                    />
                  )}
                </Field>
              )}
              {davMode === "custom" && (
                <Field label={t("moves.wizard.davUrl")}>
                  {(id) => (
                    <TextInput
                      id={id}
                      autoCapitalize="none"
                      spellCheck={false}
                      placeholder="https://dav.example.com/"
                      value={davUrl}
                      onChange={(event) => changed(setDavUrl)(event.target.value.trim())}
                    />
                  )}
                </Field>
              )}
            </div>
          )}
        </div>
        <details>
          <summary className="cursor-pointer text-[13px] font-semibold">{t("moves.wizard.pace")}</summary>
          <div className="mt-3 grid gap-3 sm:grid-cols-2">
            <Field label={t("moves.settings.parallel")} hint={t("moves.settings.parallelHint")}>
              {(id) => (
                <TextInput
                  id={id}
                  type="number"
                  min={1}
                  max={8}
                  value={parallel}
                  onChange={(event) => changed(setParallel)(event.target.value)}
                />
              )}
            </Field>
            <Field label={t("moves.settings.syncMinutes")} hint={t("moves.settings.syncMinutesHint")}>
              {(id) => (
                <TextInput
                  id={id}
                  type="number"
                  min={5}
                  max={1440}
                  value={syncMinutes}
                  onChange={(event) => changed(setSyncMinutes)(event.target.value)}
                />
              )}
            </Field>
          </div>
        </details>
      </Step>

      <Step number={3} title={kind === "domain" ? t("moves.wizard.peopleTitle") : t("moves.wizard.oldMailboxTitle")}>
        <p className="text-[13px] text-muted">
          {kind === "domain" ? t("moves.wizard.peopleExplain") : t("moves.wizard.oldMailboxExplain")}
        </p>
        <RowsEditor
          rows={rows}
          onChange={changed(setRows)}
          problems={problems}
          domain={cleanDomain}
          single={kind === "mailbox"}
        />
      </Step>

      <Step number={4} title={t("moves.wizard.checkTitle")}>
        <p className="text-[13px] text-muted">{t("moves.wizard.checkExplain")}</p>
        {plan && (
          <div className="flex flex-col gap-1.5 text-[13px]" role="status">
            {!plan.domainExists && <p>{t("moves.wizard.planDomain", { domain: cleanDomain })}</p>}
            <p>
              {t("moves.wizard.planRows", {
                created: plan.rows.filter((row) => !row.exists).length,
                filled: plan.rows.filter((row) => row.exists).length,
              })}
            </p>
            {plan.rows.some((row) => row.hasPassword) && (
              <p className="text-muted">{t("moves.wizard.planPasswords")}</p>
            )}
            {plan.problems.length > 0 ? (
              <p className="text-danger">{t("moves.wizard.planProblems", { count: plan.problems.length })}</p>
            ) : (
              <p className="flex items-center gap-1.5 text-success">
                <CheckCircle2 className="size-4" aria-hidden />
                {t("moves.wizard.planReady")}
              </p>
            )}
          </div>
        )}
        {(check.isError || start.isError) && (
          <p role="alert" className="text-[13px] text-danger">
            {errorText(check.error ?? start.error)}
          </p>
        )}
        <div className="flex flex-wrap justify-end gap-2">
          <Button busy={check.isPending} disabled={sourceMissing} onClick={() => check.mutate()}>
            {t("moves.wizard.check")}
          </Button>
          <Button
            variant="primary"
            icon={Truck}
            busy={start.isPending}
            disabled={!ready || sourceMissing}
            onClick={() => start.mutate()}
          >
            {t("moves.wizard.start")}
          </Button>
        </div>
      </Step>
    </div>
  );
}
