import { useMutation } from "@tanstack/react-query";
import clsx from "clsx";
import { FileUp, Plus, Server, Trash2 } from "lucide-react";
import { useState, type ChangeEvent } from "react";
import { Button, IconButton } from "@/components/ui/Button";
import { useT } from "@/i18n";
import { api, type MoveCsvRead, type MoveRowProblem } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { emptyRow, mergeCsv, type EditRow } from "./moves";

const COLUMNS: (keyof EditRow)[] = ["oldAddress", "login", "password", "name", "target", "quotaMb", "aliases"];

const cellClass =
  "h-9 w-full min-w-[9rem] rounded-control border border-line bg-surface px-2.5 text-[13px] text-ink placeholder:text-faint focus:border-pink focus:shadow-focus focus:outline-none";

/** The words for a row's problem: the field and what is wrong with it. */
export function useProblemText() {
  const { t } = useT();
  return (problem: { field: string; code: string }) =>
    `${t(`moves.fields.${problem.field}`, { defaultValue: problem.field })}: ${t(`moves.problems.${problem.code}`, {
      defaultValue: t("moves.problems.invalid"),
    })}`;
}

function CsvImport({
  domain,
  rows,
  onChange,
}: {
  domain: string;
  rows: EditRow[];
  onChange: (rows: EditRow[]) => void;
}) {
  const { t } = useT();
  const errorText = useErrorText();
  const problemText = useProblemText();
  const [text, setText] = useState("");
  const read = useMutation({
    mutationFn: (csv: string) =>
      api<MoveCsvRead>("/api/admin/moves/csv", { method: "POST", body: { text: csv, domain: domain.trim() || null } }),
    onSuccess: (result) => {
      onChange(mergeCsv(rows, result));
      if (result.problems.length === 0) setText("");
    },
  });
  const readFile = (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file) return;
    void file.text().then((content) => {
      setText(content);
      read.mutate(content);
    });
  };
  return (
    <details className="rounded-control border border-hairline p-3">
      <summary className="cursor-pointer text-[13px] font-semibold">{t("moves.csv.title")}</summary>
      <div className="mt-3 flex flex-col gap-2">
        <p className="text-[12px] text-muted">{t("moves.csv.explain")}</p>
        <textarea
          aria-label={t("moves.csv.paste")}
          className="min-h-28 w-full rounded-control border border-line bg-surface p-2.5 font-mono text-[12px] focus:border-pink focus:outline-none"
          placeholder={
            "Alte Adresse;Passwort;Name;Neue Adresse;Quota;Aliase\nmini@example.com;…;Mini;;2 GB;info@example.com"
          }
          spellCheck={false}
          value={text}
          onChange={(event) => setText(event.target.value)}
        />
        <div className="flex flex-wrap items-center gap-2">
          <Button size="sm" busy={read.isPending} disabled={!text.trim()} onClick={() => read.mutate(text)}>
            {t("moves.csv.read")}
          </Button>
          <label className="inline-flex h-8 cursor-pointer items-center gap-1.5 rounded-full border border-line px-3 text-[13px] font-semibold hover:bg-elevated">
            <FileUp className="size-4" aria-hidden />
            {t("moves.csv.file")}
            <input type="file" accept=".csv,.txt,text/csv,text/plain" className="sr-only" onChange={readFile} />
          </label>
        </div>
        {read.isError && (
          <p role="alert" className="text-[13px] text-danger">
            {errorText(read.error)}
          </p>
        )}
        {read.data && (
          <div className="text-[12px] text-muted" role="status">
            <p>
              {t("moves.csv.result", {
                count: read.data.rows.length,
                delimiter: read.data.delimiter === "\t" ? "Tab" : read.data.delimiter,
              })}{" "}
              {read.data.header ? t("moves.csv.withHeader") : t("moves.csv.withoutHeader")}
            </p>
            {read.data.problems.length > 0 && (
              <ul className="mt-1 list-disc pl-5 text-danger">
                {read.data.problems.map((problem, index) => (
                  <li key={index}>
                    {t("moves.csv.line", { line: problem.line })} {problemText(problem)}
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}
      </div>
    </details>
  );
}

/**
 * The people of a move as a table: old address, old login and password, name, address here,
 * quota and aliases; each row can name its own old server. A CSV list fills it.
 */
export function RowsEditor({
  rows,
  onChange,
  problems,
  domain,
  single = false,
}: {
  rows: EditRow[];
  onChange: (rows: EditRow[]) => void;
  /** By the table's row index. */
  problems: Map<number, MoveRowProblem[]>;
  domain: string;
  single?: boolean;
}) {
  const { t } = useT();
  const problemText = useProblemText();
  const [servers, setServers] = useState<Set<number>>(new Set());
  // A single move picks its mailbox above the table.
  const columns = single ? COLUMNS.filter((column) => column !== "target") : COLUMNS;
  const update = (index: number, field: keyof EditRow, value: string) =>
    onChange(rows.map((row, at) => (at === index ? { ...row, [field]: value } : row)));
  const placeholder = (field: keyof EditRow, row: EditRow) => {
    if (field === "login") return row.oldAddress || t("moves.placeholders.login");
    if (field === "target") {
      const local = row.oldAddress.split("@")[0];
      return local && domain ? `${local}@${domain}` : t("moves.placeholders.target");
    }
    return t(`moves.placeholders.${field}`);
  };
  return (
    <div className="flex flex-col gap-3">
      <div className="overflow-x-auto">
        <table className="w-full border-separate border-spacing-x-1.5 border-spacing-y-1 text-left">
          <thead>
            <tr className="text-[12px] text-muted">
              {columns.map((column) => (
                <th key={column} scope="col" className="px-1 font-semibold whitespace-nowrap">
                  {t(`moves.fields.${column}`)}
                </th>
              ))}
              <th scope="col" className="sr-only">
                {t("moves.rows.actions")}
              </th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row, index) => {
              const rowProblems = problems.get(index) ?? [];
              const bad = new Set(rowProblems.map((problem) => problem.field));
              return [
                <tr key={`row-${index}`}>
                  {columns.map((column) => (
                    <td key={column}>
                      <input
                        aria-label={`${t(`moves.fields.${column}`)} ${index + 1}`}
                        className={clsx(cellClass, bad.has(column === "quotaMb" ? "quota" : column) && "border-danger")}
                        type={column === "password" ? "password" : column === "quotaMb" ? "number" : "text"}
                        min={column === "quotaMb" ? 0 : undefined}
                        autoComplete="off"
                        autoCapitalize="none"
                        spellCheck={false}
                        placeholder={placeholder(column, row)}
                        value={row[column]}
                        onChange={(event) => update(index, column, event.target.value)}
                      />
                    </td>
                  ))}
                  <td className="whitespace-nowrap">
                    <IconButton
                      icon={Server}
                      label={t("moves.rows.servers")}
                      onClick={() =>
                        setServers((open) => {
                          const next = new Set(open);
                          if (next.has(index)) next.delete(index);
                          else next.add(index);
                          return next;
                        })
                      }
                    />
                    {!single && (
                      <IconButton
                        icon={Trash2}
                        label={t("moves.rows.remove")}
                        onClick={() => {
                          const next = rows.filter((_, at) => at !== index);
                          onChange(next.length > 0 ? next : [emptyRow()]);
                        }}
                      />
                    )}
                  </td>
                </tr>,
                servers.has(index) || bad.has("imapHost") || bad.has("davUrl") ? (
                  <tr key={`servers-${index}`}>
                    <td colSpan={columns.length + 1}>
                      <div className="grid gap-2 pb-1 sm:grid-cols-2">
                        <input
                          aria-label={`${t("moves.fields.imapHost")} ${index + 1}`}
                          className={clsx(cellClass, bad.has("imapHost") && "border-danger")}
                          placeholder={t("moves.placeholders.imapHost")}
                          spellCheck={false}
                          autoCapitalize="none"
                          value={row.imapHost}
                          onChange={(event) => update(index, "imapHost", event.target.value)}
                        />
                        <input
                          aria-label={`${t("moves.fields.davUrl")} ${index + 1}`}
                          className={clsx(cellClass, bad.has("davUrl") && "border-danger")}
                          placeholder={t("moves.placeholders.davUrl")}
                          spellCheck={false}
                          autoCapitalize="none"
                          value={row.davUrl}
                          onChange={(event) => update(index, "davUrl", event.target.value)}
                        />
                      </div>
                    </td>
                  </tr>
                ) : null,
                rowProblems.length > 0 ? (
                  <tr key={`problems-${index}`}>
                    <td colSpan={columns.length + 1} role="alert" className="px-1 pb-1 text-[12px] text-danger">
                      {rowProblems.map(problemText).join(" · ")}
                    </td>
                  </tr>
                ) : null,
              ];
            })}
          </tbody>
        </table>
      </div>
      {!single && (
        <>
          <Button size="sm" icon={Plus} className="self-start" onClick={() => onChange([...rows, emptyRow()])}>
            {t("moves.rows.add")}
          </Button>
          <CsvImport domain={domain} rows={rows} onChange={onChange} />
        </>
      )}
    </div>
  );
}
