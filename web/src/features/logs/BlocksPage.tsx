import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { LockOpen, Plus, X } from "lucide-react";
import { useState, type FormEvent } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button, IconButton } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { EmptyState } from "@/components/ui/EmptyState";
import { Field, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, type Block, type BlocksView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDateTime, formatDuration, formatRelative } from "@/lib/format";
import { toast } from "@/state/toasts";
import { looksLikeNetwork } from "./blocks";

const BLOCKS_KEY = ["admin", "blocks"];

function BlockRow({ block, now }: { block: Block; now: number }) {
  const { t, i18n } = useT();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  const unblock = useMutation({
    mutationFn: () => api<void>("/api/admin/blocks/unblock", { method: "POST", body: { ip: block.ip } }),
    onSuccess: () => {
      toast(t("logs.blocks.unblocked", { ip: block.ip }), "success");
      void queryClient.invalidateQueries({ queryKey: BLOCKS_KEY });
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const facts = [
    block.login && t("logs.blocks.tried", { login: block.login }),
    block.protocol.toUpperCase(),
    t("logs.blocks.since", { time: formatRelative(block.since, i18n.language, now) }),
    t("logs.blocks.remaining", {
      duration: formatDuration(Math.max(0, block.until - now), t),
      time: formatDateTime(block.until, i18n.language),
    }),
  ].filter(Boolean);

  return (
    <li className="flex flex-wrap items-start justify-between gap-3 rounded-card border border-hairline bg-surface p-4">
      <div className="min-w-0">
        <p className="flex flex-wrap items-center gap-2">
          <span className="font-mono text-sm font-semibold break-all">{block.ip}</span>
          {block.times >= 2 && (
            <span className="inline-flex h-6 items-center rounded-full bg-danger-tint px-2.5 text-[12px] font-semibold text-danger">
              {t("logs.blocks.repeat", { times: block.times })}
            </span>
          )}
        </p>
        <p className="mt-1 text-sm">{t(`logs.blocks.reasons.${block.reason}`)}</p>
        <p className="text-[12px] break-words text-muted">{facts.join(" · ")}</p>
      </div>
      <Button
        size="sm"
        icon={LockOpen}
        busy={unblock.isPending}
        onClick={() => {
          if (window.confirm(t("logs.blocks.unblockConfirm", { ip: block.ip }))) unblock.mutate();
        }}
      >
        {t("logs.blocks.unblock")}
      </Button>
    </li>
  );
}

/** The addresses and networks that are never blocked; every change is saved at once. */
function ExemptCard({ exempt }: { exempt: string[] }) {
  const { t } = useT();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  const [draft, setDraft] = useState("");
  const [problem, setProblem] = useState<string | null>(null);
  const save = useMutation({
    mutationFn: (networks: string[]) =>
      api<{ exempt: string[] }>("/api/admin/blocks/exempt", { method: "PUT", body: { networks } }),
    onSuccess: (result) => {
      // The server's spelling, so the list shows what is really in force.
      queryClient.setQueryData<BlocksView>(BLOCKS_KEY, (view) => view && { ...view, exempt: result.exempt });
      setDraft("");
      toast(t("logs.blocks.exempt.saved"), "success");
    },
    onError: (error) => setProblem(errorText(error)),
  });

  const add = (event: FormEvent) => {
    event.preventDefault();
    const value = draft.trim();
    if (!value) return;
    if (!looksLikeNetwork(value)) return setProblem(t("logs.blocks.exempt.invalid"));
    if (exempt.includes(value.toLowerCase())) return setProblem(t("logs.blocks.exempt.duplicate"));
    setProblem(null);
    save.mutate([...exempt, value]);
  };

  return (
    <Card title={t("logs.blocks.exempt.title")}>
      <div className="flex flex-col gap-4">
        <p className="text-sm text-muted">{t("logs.blocks.exempt.intro")}</p>
        {exempt.length === 0 ? (
          <p className="text-[13px] text-muted">{t("logs.blocks.exempt.empty")}</p>
        ) : (
          <ul className="flex flex-wrap gap-2">
            {exempt.map((network) => (
              <li
                key={network}
                className="inline-flex items-center gap-1 rounded-full bg-canvas py-0.5 pr-1 pl-3 font-mono text-[12px] break-all"
              >
                {network}
                <IconButton
                  size="sm"
                  icon={X}
                  label={t("logs.blocks.exempt.remove", { network })}
                  disabled={save.isPending}
                  onClick={() => {
                    setProblem(null);
                    save.mutate(exempt.filter((entry) => entry !== network));
                  }}
                />
              </li>
            ))}
          </ul>
        )}
        <form onSubmit={add}>
          <Field label={t("logs.blocks.exempt.label")} hint={t("logs.blocks.exempt.hint")} error={problem}>
            {(id) => (
              <div className="flex gap-2">
                <TextInput
                  id={id}
                  className="font-mono sm:max-w-80"
                  autoCapitalize="none"
                  spellCheck={false}
                  value={draft}
                  onChange={(event) => {
                    setDraft(event.target.value);
                    setProblem(null);
                  }}
                />
                <Button type="submit" icon={Plus} busy={save.isPending} disabled={!draft.trim()}>
                  {t("logs.blocks.exempt.add")}
                </Button>
              </div>
            )}
          </Field>
        </form>
      </div>
    </Card>
  );
}

/** Which addresses may not log in right now, why, and which ones never get blocked. */
export function BlocksPage() {
  const { t } = useT();
  const blocks = useQuery({
    queryKey: BLOCKS_KEY,
    queryFn: () => api<BlocksView>("/api/admin/blocks"),
    refetchInterval: 15_000,
  });

  if (blocks.isPending) return <Loading />;
  if (blocks.isError) return <LoadError error={blocks.error} onRetry={() => void blocks.refetch()} />;
  // The moment of the last answer: the remaining times move on with every refresh.
  const now = blocks.dataUpdatedAt / 1000;

  return (
    <div className="flex flex-col gap-5">
      {blocks.data.blocks.length === 0 ? (
        <EmptyState
          compact
          scene="emptyFolder"
          title={t("logs.blocks.empty.title")}
          body={t("logs.blocks.empty.body")}
        />
      ) : (
        <ul className="flex flex-col gap-3">
          {blocks.data.blocks.map((block) => (
            <BlockRow key={block.ip} block={block} now={now} />
          ))}
        </ul>
      )}
      <ExemptCard exempt={blocks.data.exempt} />
      <Card title={t("logs.blocks.rulesTitle")}>
        <ul className="flex list-disc flex-col gap-1.5 pl-5 text-sm text-muted">
          <li>{t("logs.blocks.rules.instant")}</li>
          <li>{t("logs.blocks.rules.typo")}</li>
          <li>{t("logs.blocks.rules.never")}</li>
          <li>{t("logs.blocks.rules.shared")}</li>
          <li>{t("logs.blocks.rules.short")}</li>
        </ul>
      </Card>
    </div>
  );
}
