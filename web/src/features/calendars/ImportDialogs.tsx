import { useMutation, useQueryClient } from "@tanstack/react-query";
import { CalendarPlus, CloudDownload, FileUp, Rss } from "lucide-react";
import { useState, type FormEvent } from "react";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Dialog } from "@/components/ui/Dialog";
import { Field, Segmented, Select, TextInput, Toggle } from "@/components/ui/Field";
import { useT } from "@/i18n";
import {
  api,
  type CalendarsView,
  type ImportProblem,
  type ImportReport,
  type ImportResult,
  type MirrorReport,
  type OwnCollection,
  type RemoteImportResult,
  type SubscribeResult,
} from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatBytes } from "@/lib/format";
import { toast } from "@/state/toasts";

export const calendarsKey = ["account", "calendars"] as const;

type Kind = OwnCollection["kind"];

/** How often a feed may be fetched, as the page offers it. */
export const INTERVALS = [900, 3600, 21_600, 86_400] as const;

function Problems({ problems, truncated }: { problems: ImportProblem[]; truncated?: boolean }) {
  const { t } = useT();
  if (problems.length === 0) return null;
  return (
    <details className="text-[13px]">
      <summary className="cursor-pointer text-muted">{t("calendars.import.problems")}</summary>
      <ul className="mt-1 flex max-h-48 flex-col gap-0.5 overflow-auto">
        {problems.map((problem, index) => (
          <li key={index} className="flex gap-2">
            <span className="min-w-0 flex-1 truncate">{problem.item}</span>
            <span className="shrink-0 text-muted">
              {t(`calendars.import.reasons.${problem.reason}`, t("calendars.import.reasons.invalid"))}
            </span>
          </li>
        ))}
        {truncated && <li className="text-muted">…</li>}
      </ul>
    </details>
  );
}

/** What an import did, in one line and the list of what it left out. */
export function ReportView({ report }: { report: ImportReport }) {
  const { t } = useT();
  return (
    <div className="flex flex-col gap-1.5 rounded-control bg-canvas p-3">
      <p className="text-sm">
        {t("calendars.import.summary", {
          created: report.created,
          updated: report.updated,
          unchanged: report.unchanged,
          skipped: report.skipped,
        })}
      </p>
      <Problems problems={report.problems} truncated={report.truncated} />
    </div>
  );
}

function MirrorView({ report }: { report: MirrorReport }) {
  const { t } = useT();
  return (
    <div className="flex flex-col gap-1.5 rounded-control bg-canvas p-3">
      <p className="text-sm">{t("calendars.subscribe.summary", { entries: report.entries })}</p>
      <Problems problems={report.problems} />
    </div>
  );
}

function kindOfFile(name: string): Kind | null {
  const lower = name.toLowerCase();
  if (lower.endsWith(".ics") || lower.endsWith(".ical") || lower.endsWith(".ifb")) return "calendar";
  if (lower.endsWith(".vcf") || lower.endsWith(".vcard")) return "addressbook";
  return null;
}

function FileImportDialog({ view, onClose }: { view: CalendarsView; onClose: () => void }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [file, setFile] = useState<File | null>(null);
  const [kind, setKind] = useState<Kind>(view.calendars ? "calendar" : "addressbook");
  const [target, setTarget] = useState("new");
  const [name, setName] = useState("");
  const [onlyNew, setOnlyNew] = useState(false);
  const targets = view.own.filter((collection) => collection.kind === kind && !collection.subscription);
  const run = useMutation({
    mutationFn: (chosen: File) => {
      const query = new URLSearchParams({ kind, mode: onlyNew ? "onlyNew" : "merge" });
      if (target !== "new") query.set("target", target);
      else if (name.trim()) query.set("name", name.trim());
      // The calendar's own name comes first; the file's name only when it has none.
      if (target === "new") query.set("fileName", chosen.name.replace(/\.[^.]+$/, ""));
      return api<ImportResult>(`/api/account/calendars/import?${query}`, { method: "POST", body: chosen });
    },
    onSuccess: (result) => {
      queryClient.setQueryData(calendarsKey, result);
      toast(t("calendars.import.doneToast", { name: result.collection.name }), "success");
    },
  });
  const pick = (chosen: File | null) => {
    setFile(chosen);
    run.reset();
    const guessed = chosen && kindOfFile(chosen.name);
    if (guessed && (guessed === "calendar" ? view.calendars : view.contacts)) {
      setKind(guessed);
      setTarget("new");
    }
  };
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (file) run.mutate(file);
  };
  const tooBig = file !== null && file.size > view.limits.uploadBytes;

  return (
    <form className="flex flex-col gap-4 p-5" onSubmit={submit}>
      <p className="text-sm text-muted">{t("calendars.import.fileIntro")}</p>
      <Field
        label={t("calendars.import.file")}
        hint={t("calendars.import.fileHint", { size: formatBytes(view.limits.uploadBytes, i18n.language) })}
        error={tooBig ? t("errors.codes.tooLarge") : undefined}
      >
        {(id) => (
          <input
            id={id}
            type="file"
            accept=".ics,.vcf,.ical,.vcard,text/calendar,text/vcard,text/x-vcard"
            className="text-sm file:mr-3 file:rounded-full file:border file:border-line file:bg-surface file:px-3 file:py-1.5 file:text-[13px] file:font-semibold"
            onChange={(event) => pick(event.target.files?.[0] ?? null)}
          />
        )}
      </Field>
      {view.calendars && view.contacts && (
        <Segmented
          label={t("calendars.import.kind")}
          value={kind}
          onChange={(next) => {
            setKind(next);
            setTarget("new");
          }}
          options={[
            { value: "calendar", label: t("calendars.import.kinds.calendar") },
            { value: "addressbook", label: t("calendars.import.kinds.addressbook") },
          ]}
        />
      )}
      <Field label={t("calendars.import.target")}>
        {(id) => (
          <Select id={id} value={target} onChange={(event) => setTarget(event.target.value)}>
            <option value="new">{t("calendars.import.newCollection")}</option>
            {targets.map((collection) => (
              <option key={collection.id} value={String(collection.id)}>
                {collection.name}
              </option>
            ))}
          </Select>
        )}
      </Field>
      {target === "new" && (
        <Field label={t("calendars.import.name")} hint={t("calendars.import.nameHint")}>
          {(id) => <TextInput id={id} value={name} maxLength={255} onChange={(event) => setName(event.target.value)} />}
        </Field>
      )}
      {target !== "new" && (
        <Toggle
          checked={onlyNew}
          onChange={setOnlyNew}
          label={t("calendars.import.onlyNew")}
          description={t("calendars.import.onlyNewHint")}
        />
      )}
      {run.isError && <p className="text-[13px] text-danger">{errorText(run.error)}</p>}
      {run.data && <ReportView report={run.data.report} />}
      <div className="flex justify-end gap-2">
        <Button variant="ghost" onClick={onClose}>
          {run.data ? t("common.close") : t("common.cancel")}
        </Button>
        {!run.data && (
          <Button variant="primary" type="submit" icon={FileUp} busy={run.isPending} disabled={!file || tooBig}>
            {t("calendars.import.run")}
          </Button>
        )}
      </div>
    </form>
  );
}

function SubscribeDialog({ onClose }: { onClose: () => void }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [url, setUrl] = useState("");
  const [name, setName] = useState("");
  const [interval, setInterval] = useState<number>(3600);
  const [keepAlarms, setKeepAlarms] = useState(false);
  const [once, setOnce] = useState(false);
  const subscribe = useMutation<ImportResult | SubscribeResult>({
    mutationFn: () =>
      once
        ? api<ImportResult>("/api/account/calendars/import-url", {
            method: "POST",
            body: { url: url.trim(), name: name.trim() || undefined },
          })
        : api<SubscribeResult>("/api/account/calendar-subscriptions", {
            method: "POST",
            body: { url: url.trim(), name: name.trim() || undefined, intervalSecs: interval, keepAlarms },
          }),
    onSuccess: (result) => {
      queryClient.setQueryData(calendarsKey, result);
      toast(
        t(once ? "calendars.import.doneToast" : "calendars.subscribe.doneToast", { name: result.collection.name }),
        "success",
      );
    },
  });
  const submit = (event: FormEvent) => {
    event.preventDefault();
    subscribe.mutate();
  };
  const result = subscribe.data;

  return (
    <form className="flex flex-col gap-4 p-5" onSubmit={submit}>
      <p className="text-sm text-muted">{t("calendars.subscribe.intro")}</p>
      <Field label={t("calendars.subscribe.url")} hint={t("calendars.subscribe.urlHint")}>
        {(id) => (
          <TextInput
            id={id}
            type="text"
            inputMode="url"
            required
            autoComplete="off"
            autoCapitalize="none"
            spellCheck={false}
            placeholder="https://calendar.example.org/…/basic.ics"
            value={url}
            onChange={(event) => setUrl(event.target.value)}
          />
        )}
      </Field>
      <Field label={t("calendars.import.name")} hint={t("calendars.subscribe.nameHint")}>
        {(id) => <TextInput id={id} value={name} maxLength={255} onChange={(event) => setName(event.target.value)} />}
      </Field>
      <Toggle
        checked={once}
        onChange={setOnce}
        label={t("calendars.subscribe.once")}
        description={t("calendars.subscribe.onceHint")}
      />
      {!once && (
        <>
          <Field label={t("calendars.subscribe.interval")}>
            {(id) => (
              <Select id={id} value={interval} onChange={(event) => setInterval(Number(event.target.value))}>
                {INTERVALS.map((secs) => (
                  <option key={secs} value={secs}>
                    {t(`calendars.subscribe.every.${secs}`)}
                  </option>
                ))}
              </Select>
            )}
          </Field>
          <Toggle
            checked={keepAlarms}
            onChange={setKeepAlarms}
            label={t("calendars.subscribe.keepAlarms")}
            description={t("calendars.subscribe.keepAlarmsHint")}
          />
        </>
      )}
      {subscribe.isError && <p className="text-[13px] text-danger">{errorText(subscribe.error)}</p>}
      {result &&
        ("total" in result.report ? <ReportView report={result.report} /> : <MirrorView report={result.report} />)}
      <div className="flex justify-end gap-2">
        <Button variant="ghost" onClick={onClose}>
          {result ? t("common.close") : t("common.cancel")}
        </Button>
        {!result && (
          <Button variant="primary" type="submit" icon={Rss} busy={subscribe.isPending} disabled={!url.trim()}>
            {once ? t("calendars.import.run") : t("calendars.subscribe.run")}
          </Button>
        )}
      </div>
    </form>
  );
}

/** Which hint a provider needs before its password is asked for, by the address's domain. */
function hintFor(address: string): string | null {
  const domain = address.split("@")[1]?.trim().toLowerCase() ?? "";
  if (["icloud.com", "me.com", "mac.com"].includes(domain)) return "icloud";
  if (domain === "web.de") return "webde";
  if (/^gmx\.[a-z.]+$/.test(domain)) return "gmx";
  if (["gmail.com", "googlemail.com"].includes(domain)) return "google";
  if (/^(outlook|hotmail|live|msn)\.[a-z.]+$/.test(domain)) return "outlook";
  if (["mailbox.org", "fastmail.com", "fastmail.fm"].includes(domain)) return "appPassword";
  return null;
}

function RemoteDialog({ view, onClose }: { view: CalendarsView; onClose: () => void }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [address, setAddress] = useState("");
  const [password, setPassword] = useState("");
  const [server, setServer] = useState("");
  const [showServer, setShowServer] = useState(false);
  const [calendars, setCalendars] = useState(view.calendars);
  const [contacts, setContacts] = useState(view.contacts);
  const move = useMutation({
    mutationFn: () =>
      api<RemoteImportResult>("/api/account/calendars/remote", {
        method: "POST",
        body: {
          address: address.trim(),
          password,
          server: server.trim() || undefined,
          kinds: [...(calendars ? ["calendar"] : []), ...(contacts ? ["addressbook"] : [])],
        },
      }),
    onSuccess: (result) => {
      queryClient.setQueryData(calendarsKey, result);
      // The password was for this one request; it is not kept, not even in the form.
      setPassword("");
      toast(t("calendars.remote.doneToast"), "success");
    },
  });
  const hint = hintFor(address);
  const blocked = hint === "google" || hint === "outlook";
  const submit = (event: FormEvent) => {
    event.preventDefault();
    move.mutate();
  };
  const close = () => {
    setPassword("");
    onClose();
  };

  return (
    <form className="flex flex-col gap-4 p-5" onSubmit={submit} autoComplete="off">
      <p className="text-sm text-muted">{t("calendars.remote.intro")}</p>
      <Field label={t("calendars.remote.address")}>
        {(id) => (
          <TextInput
            id={id}
            type="email"
            required
            autoComplete="off"
            autoCapitalize="none"
            spellCheck={false}
            placeholder={t("calendars.addressPlaceholder")}
            value={address}
            onChange={(event) => setAddress(event.target.value)}
          />
        )}
      </Field>
      {hint && <p className="rounded-control bg-canvas p-3 text-[13px]">{t(`calendars.remote.hints.${hint}`)}</p>}
      {!blocked && (
        <>
          <Field label={t("calendars.remote.password")} hint={t("calendars.remote.passwordHint")}>
            {(id) => (
              <TextInput
                id={id}
                type="password"
                required
                autoComplete="new-password"
                value={password}
                onChange={(event) => setPassword(event.target.value)}
              />
            )}
          </Field>
          {showServer ? (
            <Field label={t("calendars.remote.server")} hint={t("calendars.remote.serverHint")}>
              {(id) => (
                <TextInput
                  id={id}
                  type="text"
                  inputMode="url"
                  autoCapitalize="none"
                  spellCheck={false}
                  placeholder="https://dav.example.org/"
                  value={server}
                  onChange={(event) => setServer(event.target.value)}
                />
              )}
            </Field>
          ) : (
            <button
              type="button"
              className="self-start text-[13px] font-semibold text-pink-ink hover:underline"
              onClick={() => setShowServer(true)}
            >
              {t("calendars.remote.showServer")}
            </button>
          )}
          {view.calendars && (
            <Toggle checked={calendars} onChange={setCalendars} label={t("calendars.remote.calendars")} />
          )}
          {view.contacts && <Toggle checked={contacts} onChange={setContacts} label={t("calendars.remote.contacts")} />}
        </>
      )}
      {move.isPending && <p className="text-[13px] text-muted">{t("calendars.remote.working")}</p>}
      {move.isError && <p className="text-[13px] text-danger">{errorText(move.error)}</p>}
      {move.data && (
        <div className="flex flex-col gap-2">
          {move.data.provider && (
            <p className="text-[13px] text-muted">{t("calendars.remote.from", { provider: move.data.provider })}</p>
          )}
          {move.data.results.length === 0 && <p className="text-sm">{t("calendars.remote.nothing")}</p>}
          {move.data.results.map((item, index) => (
            <div key={index} className="flex flex-col gap-1">
              <p className="text-sm font-semibold">{item.name}</p>
              {item.report ? (
                <ReportView report={item.report} />
              ) : (
                <p className="text-[13px] text-danger">
                  {t(`errors.codes.${item.error ?? "internal"}`, t("errors.codes.internal"))}
                </p>
              )}
            </div>
          ))}
        </div>
      )}
      <div className="flex justify-end gap-2">
        <Button variant="ghost" onClick={close}>
          {move.data ? t("common.close") : t("common.cancel")}
        </Button>
        {!move.data && !blocked && (
          <Button
            variant="primary"
            type="submit"
            icon={CloudDownload}
            busy={move.isPending}
            disabled={!address.trim() || !password || !(calendars || contacts)}
          >
            {t("calendars.remote.run")}
          </Button>
        )}
      </div>
    </form>
  );
}

type Open = "file" | "subscribe" | "remote" | null;

/** Taking calendars and contacts over from elsewhere: files, feeds and other providers. */
export function ImportCard({ view }: { view: CalendarsView }) {
  const { t } = useT();
  const [open, setOpen] = useState<Open>(null);
  const close = () => setOpen(null);
  return (
    <Card title={t("calendars.import.title")} className="lg:col-span-2">
      <div className="flex flex-col gap-3">
        <p className="text-sm text-muted">{t("calendars.import.intro")}</p>
        <div className="flex flex-wrap gap-2">
          <Button icon={FileUp} onClick={() => setOpen("file")}>
            {t("calendars.import.fileButton")}
          </Button>
          {view.calendars && (
            <Button icon={CalendarPlus} onClick={() => setOpen("subscribe")}>
              {t("calendars.subscribe.button")}
            </Button>
          )}
          <Button icon={CloudDownload} onClick={() => setOpen("remote")}>
            {t("calendars.remote.button")}
          </Button>
        </div>
      </div>
      <Dialog open={open === "file"} onClose={close} title={t("calendars.import.fileTitle")}>
        {open === "file" && <FileImportDialog view={view} onClose={close} />}
      </Dialog>
      <Dialog open={open === "subscribe"} onClose={close} title={t("calendars.subscribe.title")}>
        {open === "subscribe" && <SubscribeDialog onClose={close} />}
      </Dialog>
      <Dialog open={open === "remote"} onClose={close} title={t("calendars.remote.title")} closeOnOutsideClick={false}>
        {open === "remote" && <RemoteDialog view={view} onClose={close} />}
      </Dialog>
    </Card>
  );
}
