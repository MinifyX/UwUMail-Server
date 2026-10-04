import { useMutation } from "@tanstack/react-query";
import {
  AlertTriangle,
  ArrowLeft,
  CheckCircle2,
  FileUp,
  Pause,
  Play,
  RefreshCw,
  Save,
  Trash2,
  UserPlus,
  X,
} from "lucide-react";
import { useState, type ChangeEvent } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button, IconButton } from "@/components/ui/Button";
import { Card, PageHeader } from "@/components/ui/Card";
import { Dialog } from "@/components/ui/Dialog";
import { Field, TextInput } from "@/components/ui/Field";
import { CHECK_STYLES } from "@/features/domains/DnsBits";
import { useT } from "@/i18n";
import {
  ApiError,
  api,
  type MoveDetail,
  type MoveLinks,
  type MoveMailboxInfo,
  type MoveMxCheck,
  type MoveRowProblem,
} from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatBytes, formatDateTime, formatNumber } from "@/lib/format";
import { Link, navigate } from "@/lib/router";
import { toast } from "@/state/toasts";
import { LeftOut, leftOutParts } from "@/features/moving/LeftOut";
import { LinksCard } from "./LinksCard";
import { MailboxStatePill, MoveStatePill, ProgressBar } from "./MoveBits";
import {
  emptyRow,
  movesPath,
  problemsByRow,
  quotaTooSmall,
  rowsToBody,
  share,
  tableIndexes,
  type EditRow,
} from "./moves";
import { useDeleteMove, useMove, useMoveAction } from "./queries";
import { RowsEditor } from "./RowsEditor";

const post = (path: string, body: unknown = {}) => api<MoveDetail>(path, { method: "POST", body });

function Overview({ detail }: { detail: MoveDetail }) {
  const { t, i18n } = useT();
  const { move } = detail;
  const { summary } = move;
  const n = (value: number) => formatNumber(value, i18n.language);
  const pause = useMoveAction(move.id, () => post(`/api/admin/moves/${move.id}/pause`), t("moves.toasts.paused"));
  const resume = useMoveAction(move.id, () => post(`/api/admin/moves/${move.id}/resume`), t("moves.toasts.resumed"));
  return (
    <Card
      title={t("moves.detail.overview")}
      action={
        move.state === "active" ? (
          <Button size="sm" icon={Pause} busy={pause.isPending} onClick={() => pause.mutate(undefined)}>
            {t("moves.actions.pauseAll")}
          </Button>
        ) : move.state === "paused" ? (
          <Button size="sm" icon={Play} busy={resume.isPending} onClick={() => resume.mutate(undefined)}>
            {t("moves.actions.resumeAll")}
          </Button>
        ) : null
      }
    >
      <div className="flex flex-col gap-3">
        <ProgressBar
          value={share(summary.messagesDone, summary.messagesTotal)}
          label={t("moves.progress.label", { domain: move.domain })}
        />
        <p className="text-[13px]">
          {t("moves.detail.messages", {
            done: n(summary.messagesDone),
            total: n(summary.messagesTotal),
            size: formatBytes(summary.bytesDone, i18n.language),
          })}
          {leftOutParts(t, summary).map((part) => ` · ${part}`)}
        </p>
        <p className="text-[13px] text-muted">
          {t("moves.detail.davCounts", { contacts: n(summary.contactsDone), events: n(summary.eventsDone) })}
        </p>
        <ul className="flex flex-wrap gap-x-4 gap-y-1 text-[13px] text-muted">
          {(["queued", "running", "synced", "paused", "done"] as const).map((state) => (
            <li key={state}>
              {t(`moves.mailboxState.${state}`)}: <span className="font-semibold text-ink">{n(summary[state])}</span>
            </li>
          ))}
        </ul>
        <p className="text-[12px] text-muted">
          {t("moves.detail.source", { host: `${move.imapHost}:${move.imapPort}` })} ·{" "}
          {move.contacts || move.calendars ? t(`moves.davModes.${move.davMode}`) : t("moves.detail.noDav")}
        </p>
      </div>
    </Card>
  );
}

function SettingsCard({ detail }: { detail: MoveDetail }) {
  const { t } = useT();
  const { move, limits } = detail;
  const [parallel, setParallel] = useState(String(move.parallel));
  const [syncMinutes, setSyncMinutes] = useState(String(move.syncMinutes));
  const save = useMoveAction(
    move.id,
    () =>
      api<MoveDetail>(`/api/admin/moves/${move.id}`, {
        method: "PATCH",
        body: { parallel: Number(parallel), syncMinutes: Number(syncMinutes) },
      }),
    t("moves.toasts.saved"),
  );
  if (move.state === "done") return null;
  const changed = Number(parallel) !== move.parallel || Number(syncMinutes) !== move.syncMinutes;
  return (
    <Card title={t("moves.settings.title")}>
      <div className="grid gap-3 sm:grid-cols-[1fr_1fr_auto] sm:items-end">
        <Field label={t("moves.settings.parallel")} hint={t("moves.settings.parallelHint")}>
          {(id) => (
            <TextInput
              id={id}
              type="number"
              min={1}
              max={limits.maxParallel}
              value={parallel}
              onChange={(event) => setParallel(event.target.value)}
            />
          )}
        </Field>
        <Field label={t("moves.settings.syncMinutes")} hint={t("moves.settings.syncMinutesHint")}>
          {(id) => (
            <TextInput
              id={id}
              type="number"
              min={limits.minSyncMinutes}
              max={limits.maxSyncMinutes}
              value={syncMinutes}
              onChange={(event) => setSyncMinutes(event.target.value)}
            />
          )}
        </Field>
        <Button
          icon={Save}
          className="sm:mb-[22px]"
          disabled={!changed}
          busy={save.isPending}
          onClick={() => save.mutate(undefined)}
        >
          {t("moves.settings.save")}
        </Button>
      </div>
    </Card>
  );
}

/** The MX switch and finishing the move. */
function FinishCard({ detail }: { detail: MoveDetail }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const { move } = detail;
  const [confirm, setConfirm] = useState<"last" | "skip" | null>(null);
  const mx = useMutation({
    mutationFn: () => api<MoveMxCheck>(`/api/admin/moves/${move.id}/mx`, { method: "POST", body: {} }),
    onError: (error) => toast(errorText(error), "error"),
  });
  const finish = useMoveAction(
    move.id,
    (skipLastRound: boolean) => post(`/api/admin/moves/${move.id}/finish`, { skipLastRound }),
    t("moves.toasts.finishing"),
  );
  if (move.state === "done") {
    return (
      <Card title={t("moves.finish.title")}>
        <p className="flex items-center gap-1.5 text-[13px] text-success">
          <CheckCircle2 className="size-4" aria-hidden />
          {move.finishedAt
            ? t("moves.finish.doneAt", { date: formatDateTime(move.finishedAt, i18n.language) })
            : t("moves.finish.done")}
        </p>
      </Card>
    );
  }
  const status = mx.data?.check.status;
  const style = status ? CHECK_STYLES[status] : null;
  return (
    <Card title={t("moves.finish.title")}>
      <div className="flex flex-col gap-3">
        <p className="text-[13px] text-muted">
          {t("moves.finish.explain", { hostname: detail.hostname, domain: move.domain })}
        </p>
        <div className="flex flex-wrap items-center gap-2">
          <Button size="sm" icon={RefreshCw} busy={mx.isPending} onClick={() => mx.mutate()}>
            {t("moves.finish.checkMx")}
          </Button>
          {style && status && (
            <span
              className={`inline-flex h-6 items-center gap-1 rounded-full px-2.5 text-[12px] font-semibold ${style.className}`}
            >
              <style.icon className="size-3.5" aria-hidden />
              {t(`moves.finish.mx.${status}`)}
            </span>
          )}
        </div>
        {mx.data && (
          <p className="text-[12px] text-muted" role="status">
            {mx.data.check.found.length > 0
              ? t("moves.finish.mxFound", { found: mx.data.check.found.join(", ") })
              : t("moves.finish.mxNone")}{" "}
            {status === "ok" ? t("moves.finish.mxReady") : t("moves.finish.mxWait", { hostname: mx.data.hostname })}
          </p>
        )}
        {move.state === "finishing" ? (
          <div className="flex flex-col gap-2">
            <p className="text-[13px] text-pink-ink">{t("moves.finish.running")}</p>
            <Button size="sm" variant="danger" className="self-start" onClick={() => setConfirm("skip")}>
              {t("moves.finish.skip")}
            </Button>
          </div>
        ) : (
          <Button variant="primary" icon={CheckCircle2} className="self-start" onClick={() => setConfirm("last")}>
            {t("moves.finish.button")}
          </Button>
        )}
      </div>
      <Dialog
        open={confirm !== null}
        onClose={() => setConfirm(null)}
        title={t("moves.finish.confirmTitle")}
        width="sm"
      >
        <div className="flex flex-col gap-4 px-6 pb-6">
          <p className="text-[13px] text-muted">
            {confirm === "skip" ? t("moves.finish.confirmSkip") : t("moves.finish.confirmLast")}
          </p>
          {status !== "ok" && confirm === "last" && (
            <p className="flex items-start gap-1.5 text-[13px] text-warning">
              <AlertTriangle className="mt-0.5 size-4 shrink-0" aria-hidden />
              {t("moves.finish.confirmMx")}
            </p>
          )}
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setConfirm(null)}>
              {t("common.cancel")}
            </Button>
            <Button
              variant={confirm === "skip" ? "danger" : "primary"}
              busy={finish.isPending}
              onClick={() => finish.mutate(confirm === "skip", { onSettled: () => setConfirm(null) })}
            >
              {confirm === "skip" ? t("moves.finish.skip") : t("moves.finish.button")}
            </Button>
          </div>
        </div>
      </Dialog>
    </Card>
  );
}

function MailboxRow({ detail, mailbox }: { detail: MoveDetail; mailbox: MoveMailboxInfo }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const id = detail.move.id;
  const [retrying, setRetrying] = useState(false);
  const [password, setPassword] = useState("");
  const [login, setLogin] = useState("");
  const retry = useMoveAction(
    id,
    () =>
      post(`/api/admin/moves/${id}/mailboxes/${mailbox.id}/retry`, {
        password: password || null,
        login: login || null,
      }),
    t("moves.toasts.queued"),
  );
  const pause = useMoveAction(id, () => post(`/api/admin/moves/${id}/mailboxes/${mailbox.id}/pause`));
  const remove = useMoveAction(
    id,
    () => api<MoveDetail>(`/api/admin/moves/${id}/mailboxes/${mailbox.id}`, { method: "DELETE" }),
    t("moves.toasts.removed"),
  );
  const upload = useMutation({
    mutationFn: ({ kind, file }: { kind: "calendar" | "addressbook"; file: File }) =>
      api<{ report: { created: number; updated: number; unchanged: number } }>(
        `/api/admin/moves/${id}/mailboxes/${mailbox.id}/import?kind=${kind}`,
        { method: "POST", body: file },
      ),
    onSuccess: (result) => {
      const count = result.report.created + result.report.updated + result.report.unchanged;
      toast(t("moves.toasts.uploaded", { count }), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const pick = (kind: "calendar" | "addressbook") => (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (file) upload.mutate({ kind, file });
  };
  const n = (value: number) => formatNumber(value, i18n.language);
  const active = mailbox.state === "queued" || mailbox.state === "running";
  const loginRefused = mailbox.state === "paused" && mailbox.error === "loginRefused";
  return (
    <li className="flex flex-col gap-2 border-t border-hairline py-3 first:border-t-0">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="min-w-0">
          <span className="font-semibold">{mailbox.address}</span>
          {mailbox.displayName && <span className="ml-1.5 text-[12px] text-muted">{mailbox.displayName}</span>}
          <span className="block text-[12px] text-muted">
            {t("moves.mailbox.from", { address: mailbox.oldAddress, login: mailbox.login })}
            {mailbox.imapHost && ` · ${mailbox.imapHost}`}
            {" · "}
            {mailbox.createdAccount ? t("moves.mailbox.created") : t("moves.mailbox.filled")}
          </span>
        </span>
        <span className="flex items-center gap-1.5">
          {mailbox.finalRound && mailbox.state !== "done" && (
            <span className="text-[12px] font-semibold text-pink-ink">{t("moves.mailbox.lastRound")}</span>
          )}
          <MailboxStatePill state={mailbox.state} />
        </span>
      </div>
      <ProgressBar
        value={share(mailbox.messagesDone, mailbox.messagesTotal)}
        label={t("moves.mailbox.progress", { address: mailbox.address })}
      />
      <p className="text-[12px] text-muted">
        {t("moves.mailbox.messages", {
          done: n(mailbox.messagesDone),
          total: n(mailbox.messagesTotal),
          size: formatBytes(mailbox.bytesDone, i18n.language),
        })}
        {mailbox.foldersTotal > 0 &&
          ` · ${t("moves.mailbox.folders", { done: mailbox.foldersDone, total: mailbox.foldersTotal })}`}
        {" · "}
        {t("moves.detail.davCounts", { contacts: n(mailbox.contactsDone), events: n(mailbox.eventsDone) })}
        {mailbox.state === "synced" &&
          mailbox.nextSyncAt &&
          ` · ${t("moves.mailbox.next", { date: formatDateTime(mailbox.nextSyncAt, i18n.language) })}`}
      </p>
      <LeftOut counts={mailbox} path={`/api/admin/moves/${id}/mailboxes/${mailbox.id}/skipped`} />
      {mailbox.aliases.length > 0 && (
        <p className="text-[12px] text-muted">{t("moves.mailbox.aliases", { aliases: mailbox.aliases.join(", ") })}</p>
      )}
      {quotaTooSmall(mailbox) && mailbox.sourceBytes !== null && (
        <p className="flex items-start gap-1.5 text-[12px] text-warning">
          <AlertTriangle className="mt-0.5 size-3.5 shrink-0" aria-hidden />
          {t("moves.mailbox.quotaWarning", {
            source: formatBytes(mailbox.sourceBytes, i18n.language),
            quota: formatBytes(mailbox.quotaBytes, i18n.language),
          })}
        </p>
      )}
      {mailbox.state === "paused" && (
        <p className="flex items-start gap-1.5 text-[12px] text-warning">
          <AlertTriangle className="mt-0.5 size-3.5 shrink-0" aria-hidden />
          <span>
            {t(`moves.errors.${mailbox.error || "failed"}`, { defaultValue: t("moves.errors.failed") })}
            {mailbox.errorDetail && mailbox.error !== "stopped" && (
              <span className="block text-muted">{mailbox.errorDetail}</span>
            )}
          </span>
        </p>
      )}
      {mailbox.davError && (
        <p className="text-[12px] text-muted">
          {t("moves.mailbox.davError", {
            reason: t(`moves.davErrors.${mailbox.davError}`, { defaultValue: t("moves.davErrors.providerError") }),
          })}
        </p>
      )}
      {retrying && (
        <div className="grid gap-2 sm:grid-cols-2">
          <Field label={t("moves.mailbox.newLogin")}>
            {(fieldId) => (
              <TextInput
                id={fieldId}
                autoComplete="off"
                spellCheck={false}
                placeholder={mailbox.login}
                value={login}
                onChange={(event) => setLogin(event.target.value)}
              />
            )}
          </Field>
          <Field label={t("moves.mailbox.newPassword")}>
            {(fieldId) => (
              <TextInput
                id={fieldId}
                type="password"
                autoComplete="off"
                value={password}
                onChange={(event) => setPassword(event.target.value)}
              />
            )}
          </Field>
        </div>
      )}
      {mailbox.state !== "done" && (
        <div className="flex flex-wrap justify-end gap-1.5">
          {active && (
            <Button
              size="sm"
              variant="ghost"
              icon={Pause}
              busy={pause.isPending}
              onClick={() => pause.mutate(undefined)}
            >
              {t("moves.actions.pause")}
            </Button>
          )}
          {(mailbox.state === "paused" || mailbox.state === "synced") && (
            <>
              {!retrying && (loginRefused || mailbox.state === "paused") && (
                <Button size="sm" variant="ghost" onClick={() => setRetrying(true)}>
                  {t("moves.actions.changeLogin")}
                </Button>
              )}
              <Button
                size="sm"
                icon={mailbox.state === "paused" ? Play : RefreshCw}
                busy={retry.isPending}
                onClick={() => retry.mutate(undefined, { onSuccess: () => setRetrying(false) })}
              >
                {mailbox.state === "paused" ? t("moves.actions.retry") : t("moves.actions.syncNow")}
              </Button>
            </>
          )}
          {(["addressbook", "calendar"] as const).map((kind) => (
            <label
              key={kind}
              className="inline-flex h-8 cursor-pointer items-center gap-1.5 rounded-full px-3 text-[13px] font-semibold hover:bg-pink-tint/60"
            >
              <FileUp className="size-4" aria-hidden />
              {t(`moves.actions.upload.${kind}`)}
              <input
                type="file"
                className="sr-only"
                accept={kind === "calendar" ? ".ics,text/calendar" : ".vcf,text/vcard"}
                onChange={pick(kind)}
              />
            </label>
          ))}
          <IconButton
            icon={Trash2}
            label={t("moves.actions.removeMailbox", { address: mailbox.address })}
            onClick={() => {
              if (window.confirm(t("moves.actions.removeConfirm", { address: mailbox.address })))
                remove.mutate(undefined);
            }}
          />
        </div>
      )}
    </li>
  );
}

function AddPeople({ detail }: { detail: MoveDetail }) {
  const { t } = useT();
  const [open, setOpen] = useState(false);
  const [rows, setRows] = useState<EditRow[]>([emptyRow()]);
  const [problems, setProblems] = useState<Map<number, MoveRowProblem[]>>(new Map());
  const add = useMoveAction(
    detail.move.id,
    () =>
      api<MoveDetail>(`/api/admin/moves/${detail.move.id}/mailboxes`, {
        method: "POST",
        body: { rows: rowsToBody(rows) },
      }),
    t("moves.toasts.added"),
  );
  if (detail.move.kind !== "domain" || detail.move.state === "finishing" || detail.move.state === "done") return null;
  if (!open) {
    return (
      <Button icon={UserPlus} className="self-start" onClick={() => setOpen(true)}>
        {t("moves.add.button")}
      </Button>
    );
  }
  return (
    <Card
      title={t("moves.add.title")}
      action={<IconButton icon={X} label={t("common.close")} onClick={() => setOpen(false)} />}
    >
      <div className="flex flex-col gap-3">
        <RowsEditor rows={rows} onChange={setRows} problems={problems} domain={detail.move.domain} />
        <Button
          variant="primary"
          className="self-end"
          busy={add.isPending}
          onClick={() =>
            add.mutate(undefined, {
              onSuccess: () => {
                setRows([emptyRow()]);
                setProblems(new Map());
                setOpen(false);
              },
              onError: (error) => {
                if (!(error instanceof ApiError) || !Array.isArray(error.blockers)) return;
                const blockers = error.blockers as MoveRowProblem[];
                const indexes = tableIndexes(rows);
                setProblems(new Map([...problemsByRow(blockers)].map(([row, list]) => [indexes[row] ?? row, list])));
              },
            })
          }
        >
          {t("moves.add.save")}
        </Button>
      </div>
    </Card>
  );
}

/** One move: how far everyone got, the MX switch, finishing, and the password links. */
export function MovePage({ id }: { id: number }) {
  const { t } = useT();
  const move = useMove(id);
  const [links, setLinks] = useState<MoveLinks | null>(null);
  const remove = useDeleteMove(id, () => navigate(movesPath));
  if (move.isPending) return <Loading />;
  if (move.isError) return <LoadError error={move.error} onRetry={() => void move.refetch()} />;
  const detail = move.data;
  return (
    <div className="flex flex-col gap-5">
      <Link
        to={movesPath}
        className="inline-flex items-center gap-1.5 self-start text-[13px] font-semibold text-pink-ink"
      >
        <ArrowLeft className="size-4" aria-hidden />
        {t("moves.wizard.back")}
      </Link>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <PageHeader
          title={t("moves.detail.title", { domain: detail.move.domain })}
          intro={t(`moves.kind.${detail.move.kind}`)}
        />
        <MoveStatePill state={detail.move.state} />
      </div>
      <Overview detail={detail} />
      <FinishCard detail={detail} />
      <Card title={t("moves.detail.mailboxes", { count: detail.mailboxes.length })}>
        <ul>
          {detail.mailboxes.map((mailbox) => (
            <MailboxRow key={mailbox.id} detail={detail} mailbox={mailbox} />
          ))}
        </ul>
      </Card>
      <AddPeople detail={detail} />
      <LinksCard
        moveId={id}
        domain={detail.move.domain}
        mailboxes={detail.mailboxes}
        links={links}
        onLinks={setLinks}
      />
      <SettingsCard key={`${detail.move.parallel}-${detail.move.syncMinutes}`} detail={detail} />
      <div className="flex justify-end">
        <Button
          variant="danger"
          icon={Trash2}
          busy={remove.isPending}
          onClick={() => {
            if (window.confirm(t("moves.actions.deleteConfirm", { domain: detail.move.domain }))) remove.mutate();
          }}
        >
          {t("moves.actions.delete")}
        </Button>
      </div>
    </div>
  );
}
