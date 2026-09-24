import { useMutation } from "@tanstack/react-query";
import { FileUp } from "lucide-react";
import { useRef, useState, type FormEvent } from "react";
import { Button } from "@/components/ui/Button";
import { Dialog } from "@/components/ui/Dialog";
import { Field, Segmented, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, type RulesImport } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatNumber } from "@/lib/format";
import { ExpiryField } from "./ExpiryField";
import { ScopePicker } from "./ScopePicker";

const textareaClass =
  "w-full rounded-control border border-line bg-surface px-3.5 py-2.5 font-mono text-[13px] text-ink placeholder:text-faint focus:border-pink focus:shadow-focus focus:outline-none";

/** How many lines a pasted text or a file may have; the server says the same. */
const MAX_LINES = 20_000;

export function ImportReportView({ report }: { report: RulesImport }) {
  const { t } = useT();
  return (
    <div className="flex flex-col gap-1 rounded-control bg-canvas px-3 py-2.5 text-[13px]">
      <p>
        {t("spam.words.imported", {
          added: report.added,
          duplicates: report.duplicates,
          refused: report.refusedCount,
        })}
      </p>
      {report.refused.length > 0 && (
        <ul className="flex flex-col gap-0.5 text-[12px] text-muted">
          {report.refused.map((refused) => (
            <li key={refused.line} className="break-all">
              <code className="text-ink">{refused.line}</code> · {refused.reason}
            </li>
          ))}
          {report.refusedCount > report.refused.length && (
            <li>{t("spam.words.moreRefused", { count: report.refusedCount - report.refused.length })}</li>
          )}
        </ul>
      )}
    </div>
  );
}

/**
 * Many rules at once: pasted, or read from a text or CSV file, one per line. Senders may carry
 * `allow` / `block` and a note after a comma, so an export can come back in.
 */
export function ImportDialog({
  open,
  onClose,
  onImported,
  admin,
  base,
  initialScope,
}: {
  open: boolean;
  onClose: () => void;
  onImported: () => void;
  admin: boolean;
  base: string;
  initialScope: string;
}) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const file = useRef<HTMLInputElement>(null);
  const [type, setType] = useState<"sender" | "word">("sender");
  const [list, setList] = useState<"allow" | "block">("block");
  const [scope, setScope] = useState(initialScope);
  const [text, setText] = useState("");
  const [note, setNote] = useState("");
  const [expiresAt, setExpiresAt] = useState<number | null>(null);
  const [report, setReport] = useState<RulesImport | null>(null);

  const lines = text.split("\n").filter((line) => line.trim() && !line.trim().startsWith("#")).length;
  const send = useMutation({
    mutationFn: () =>
      api<RulesImport>(`${base}/rules/import`, {
        method: "POST",
        body: { type, list, text, note, expiresAt, ...(admin ? { scope } : {}) },
      }),
    onSuccess: (answer) => {
      setReport(answer);
      if (answer.added > 0) {
        setText("");
        onImported();
      }
    },
  });

  const readFile = async (chosen: File | undefined) => {
    if (!chosen) return;
    const content = await chosen.text();
    // An export has a header and the value in the fourth column; anything else is one value per line.
    const rows = content.split(/\r?\n/);
    if (rows[0]?.startsWith("type,list,kind,value")) {
      const values = rows
        .slice(1)
        .map((row) => row.split(","))
        .filter((cells) => cells[0] === type && cells[3])
        .map((cells) => (type === "sender" ? `${cells[3]},${cells[1]}` : cells[3]));
      setText(values.join("\n"));
    } else {
      setText(content);
    }
  };

  const submit = (event: FormEvent) => {
    event.preventDefault();
    send.mutate();
  };

  return (
    <Dialog open={open} onClose={onClose} title={t("spam.rules.import.title")} width="lg" closeOnOutsideClick={!text}>
      <form className="flex flex-col gap-4 px-6 pt-2 pb-6" onSubmit={submit}>
        <p className="-mt-1 text-[13px] text-muted">{t("spam.rules.import.intro")}</p>
        <div className="flex flex-wrap items-center gap-3">
          <Segmented<"sender" | "word">
            label={t("spam.rules.dialog.type")}
            value={type}
            onChange={setType}
            options={[
              { value: "sender", label: t("spam.rules.types.sender") },
              { value: "word", label: t("spam.rules.types.word") },
            ]}
          />
          {type === "sender" && (
            <Segmented<"allow" | "block">
              label={t("spam.senders.list")}
              value={list}
              onChange={setList}
              options={[
                { value: "block", label: t("spam.rules.lists.block") },
                { value: "allow", label: t("spam.rules.lists.allow") },
              ]}
            />
          )}
        </div>
        <Field
          label={t("spam.rules.import.lines", { count: lines, formatted: formatNumber(lines, i18n.language) })}
          hint={type === "sender" ? t("spam.rules.import.senderHint") : t("spam.words.entriesHint")}
          error={
            lines > MAX_LINES
              ? t("spam.rules.import.tooMany", { max: formatNumber(MAX_LINES, i18n.language) })
              : undefined
          }
        >
          {(id) => (
            <textarea
              id={id}
              rows={10}
              spellCheck={false}
              className={textareaClass}
              placeholder={type === "sender" ? t("spam.rules.import.senderPlaceholder") : t("spam.words.placeholder")}
              value={text}
              onChange={(event) => setText(event.target.value)}
            />
          )}
        </Field>
        <div>
          <input
            ref={file}
            type="file"
            accept=".txt,.csv,.map,text/plain,text/csv"
            className="hidden"
            onChange={(event) => void readFile(event.target.files?.[0])}
          />
          <Button size="sm" icon={FileUp} onClick={() => file.current?.click()}>
            {t("spam.rules.import.file")}
          </Button>
        </div>
        <div className="grid gap-4 sm:grid-cols-2">
          {admin && (
            <Field label={t("spam.senders.scope")}>
              {() => <ScopePicker value={scope} onChange={setScope} label={t("spam.senders.scope")} />}
            </Field>
          )}
          <ExpiryField value={expiresAt} onChange={setExpiresAt} />
          <Field label={t("spam.senders.note")} className={admin ? "sm:col-span-2" : undefined}>
            {(id) => (
              <TextInput
                id={id}
                maxLength={200}
                placeholder={t("spam.rules.import.notePlaceholder")}
                value={note}
                onChange={(event) => setNote(event.target.value)}
              />
            )}
          </Field>
        </div>
        {report && <ImportReportView report={report} />}
        {send.isError && (
          <p role="alert" className="rounded-control bg-danger-tint px-3 py-2 text-[13px] text-danger">
            {errorText(send.error)}
          </p>
        )}
        <div className="flex flex-wrap justify-end gap-2">
          <Button onClick={onClose}>{report ? t("common.close") : t("common.cancel")}</Button>
          <Button type="submit" variant="primary" busy={send.isPending} disabled={lines === 0 || lines > MAX_LINES}>
            {t("spam.rules.import.submit", { count: lines })}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
