import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import clsx from "clsx";
import { Check, ImageUp, Inbox, Trash2 } from "lucide-react";
import { useEffect, useRef, useState, type CSSProperties } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Field, TextInput } from "@/components/ui/Field";
import { LogoSymbol } from "@/components/ui/Logo";
import { useT } from "@/i18n";
import { api, type SettingsView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { DEFAULT_BRAND, useBrand, type Brand } from "@/state/brand";
import { toast } from "@/state/toasts";
import { LockedHint, Section, ToggleField, type Form } from "./SettingsPage";

/** Colours to start from; the first is UwUMail's own pink. */
const PRESETS = [
  { key: "pink", color: "#ff4d8d" },
  { key: "violet", color: "#8b5cf6" },
  { key: "blue", color: "#2563eb" },
  { key: "teal", color: "#0d9488" },
  { key: "green", color: "#16a34a" },
  { key: "orange", color: "#ea580c" },
  { key: "red", color: "#dc2626" },
  { key: "slate", color: "#475569" },
] as const;

const UWUMAIL_PINK = PRESETS[0].color;
const HEX = /^#[0-9a-f]{6}$/i;

type Palette = { light: Record<string, string>; dark: Record<string, string> };

/** Makes the brand known again everywhere: the login page, the portal and this preview. */
function useRefreshBrand() {
  const queryClient = useQueryClient();
  return () => {
    void queryClient.invalidateQueries({ queryKey: ["info"] });
    void queryClient.invalidateQueries({ queryKey: ["session"] });
  };
}

function NameField({ form }: { form: Form }) {
  const { t } = useT();
  const locked = form.locked("brand.name");
  return (
    <Field label={t("branding.name.label")} hint={locked ? <LockedHint /> : t("branding.name.hint")}>
      {(id) => (
        <TextInput
          id={id}
          disabled={locked}
          maxLength={40}
          placeholder={DEFAULT_BRAND.name}
          value={String(form.value("brand.name") ?? "")}
          // Spaces inside the name are part of it; only the ends are tidied by the server.
          onChange={(event) => form.set("brand.name", event.target.value === "" ? null : event.target.value)}
        />
      )}
    </Field>
  );
}

function ColorField({ form }: { form: Form }) {
  const { t } = useT();
  const locked = form.locked("brand.color");
  const current = String(form.value("brand.color") ?? "").toLowerCase();
  const shown = current || UWUMAIL_PINK;
  const [text, setText] = useState(shown);
  // The text follows a swatch or the picker; typing only counts once it is a whole colour.
  const [lastShown, setLastShown] = useState(shown);
  if (shown !== lastShown) {
    setLastShown(shown);
    setText(shown);
  }
  const choose = (color: string) =>
    form.set("brand.color", color.toLowerCase() === UWUMAIL_PINK ? null : color.toLowerCase());

  return (
    <Field label={t("branding.color.label")} hint={locked ? <LockedHint /> : t("branding.color.hint")}>
      {(id) => (
        <div className={clsx("flex flex-col gap-3", locked && "pointer-events-none opacity-60")}>
          <div role="radiogroup" aria-label={t("branding.color.presets")} className="flex flex-wrap gap-2">
            {PRESETS.map((preset) => {
              const selected = shown === preset.color;
              return (
                <button
                  key={preset.key}
                  type="button"
                  role="radio"
                  aria-checked={selected}
                  aria-label={t(`branding.color.preset.${preset.key}`)}
                  title={t(`branding.color.preset.${preset.key}`)}
                  onClick={() => choose(preset.color)}
                  className={clsx(
                    "flex size-9 items-center justify-center rounded-full ring-offset-2 ring-offset-surface transition-shadow",
                    selected ? "ring-2 ring-ink" : "hover:ring-2 hover:ring-line",
                  )}
                  style={{ backgroundColor: preset.color }}
                >
                  {selected && <Check className="size-4 text-white" strokeWidth={3} aria-hidden />}
                </button>
              );
            })}
          </div>
          <div className="flex items-center gap-2">
            <input
              type="color"
              aria-label={t("branding.color.picker")}
              value={shown}
              onChange={(event) => choose(event.target.value)}
              className="h-11 w-14 shrink-0 cursor-pointer rounded-control border border-line bg-surface p-1"
            />
            <TextInput
              id={id}
              className="max-w-40 font-mono"
              spellCheck={false}
              value={text}
              onChange={(event) => {
                const value = event.target.value.trim();
                setText(value);
                if (HEX.test(value)) choose(value);
              }}
            />
          </div>
        </div>
      )}
    </Field>
  );
}

/** A small piece of the portal in the chosen colours, light and dark side by side. */
function Preview({ color, name, mascot }: { color: string | null; name: string; mascot: boolean }) {
  const { t } = useT();
  const [debounced, setDebounced] = useState(color);
  useEffect(() => {
    const timer = window.setTimeout(() => setDebounced(color), 250);
    return () => window.clearTimeout(timer);
  }, [color]);
  const palette = useQuery({
    queryKey: ["admin", "branding", "palette", debounced],
    queryFn: () => api<Palette>(`/api/admin/branding/palette?color=${encodeURIComponent(debounced ?? "")}`),
    enabled: debounced !== null,
    staleTime: Infinity,
  });
  const brandName = name.trim() || DEFAULT_BRAND.name;

  const pane = (theme: "light" | "dark") => {
    const tokens = debounced === null ? {} : (palette.data?.[theme] ?? {});
    const neutrals =
      theme === "dark"
        ? { "--uwu-surface": "#1c171f", "--uwu-canvas": "#141016", "--uwu-ink": "#f8f2f6", "--uwu-muted": "#b3a8b3" }
        : {};
    const pink = theme === "dark" && debounced === null ? DARK_PINK : {};
    return (
      <div
        className="flex flex-1 flex-col gap-3 rounded-card border border-hairline bg-surface p-4 text-ink"
        style={{ ...neutrals, ...pink, ...tokens } as CSSProperties}
      >
        <span className="flex items-center gap-2 text-[15px] font-extrabold">
          <LogoSymbol className="h-6 w-auto max-w-12" mascot={mascot} />
          <span className="truncate">{brandName}</span>
        </span>
        <span className="flex h-9 items-center gap-2 rounded-full bg-pink-tint px-3 text-[13px] font-semibold text-pink-ink">
          <Inbox className="size-4" aria-hidden />
          {t("branding.preview.inbox")}
        </span>
        <div className="flex flex-wrap items-center gap-2">
          <span className="inline-flex h-9 items-center rounded-full bg-pink-solid px-4 text-[13px] font-bold text-on-pink">
            {t("branding.preview.button")}
          </span>
          <span className="text-[13px] font-semibold text-pink-ink underline">{t("branding.preview.link")}</span>
        </div>
        <span className="text-[12px] text-muted">{t(`branding.preview.${theme}`)}</span>
      </div>
    );
  };

  return (
    <div className="flex flex-col gap-3 sm:flex-row" aria-label={t("branding.preview.label")} role="group">
      {pane("light")}
      {pane("dark")}
    </div>
  );
}

/** The built-in dark accent, for the dark pane while the pink is chosen. */
const DARK_PINK = {
  "--uwu-pink": "#ff7fac",
  "--uwu-pink-solid": "#ff7fac",
  "--uwu-on-pink": "#1c1420",
  "--uwu-pink-ink": "#ffa3c4",
  "--uwu-pink-tint": "#3a1a2a",
};

function LogoCard() {
  const { t } = useT();
  const errorText = useErrorText();
  const refresh = useRefreshBrand();
  const apply = useBrand((s) => s.apply);
  const logo = useBrand((s) => s.logo);
  const input = useRef<HTMLInputElement>(null);
  const done = (brand: Brand) => {
    apply(brand);
    refresh();
  };
  const upload = useMutation({
    mutationFn: (file: File) => api<Brand>("/api/admin/branding/logo", { method: "PUT", body: file }),
    onSuccess: (brand) => {
      done(brand);
      toast(t("branding.logo.saved"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const remove = useMutation({
    mutationFn: () => api<Brand>("/api/admin/branding/logo", { method: "DELETE" }),
    onSuccess: (brand) => {
      done(brand);
      toast(t("branding.logo.removed"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });

  return (
    <Card title={t("branding.logo.title")}>
      <div className="flex flex-col gap-4">
        <p className="-mt-1 text-[13px] text-muted">{t("branding.logo.intro")}</p>
        <div className="flex flex-wrap items-center gap-4">
          <div className="flex size-20 shrink-0 items-center justify-center rounded-card border border-hairline bg-canvas p-2">
            <LogoSymbol className="max-h-full w-auto max-w-full" title={t("branding.logo.current")} />
          </div>
          <div className="flex flex-wrap gap-2">
            <input
              ref={input}
              type="file"
              accept="image/png,image/jpeg,image/webp,image/svg+xml,.svg"
              className="hidden"
              onChange={(event) => {
                const file = event.target.files?.[0];
                event.target.value = "";
                if (file) upload.mutate(file);
              }}
            />
            <Button icon={ImageUp} busy={upload.isPending} onClick={() => input.current?.click()}>
              {t(logo ? "branding.logo.replace" : "branding.logo.upload")}
            </Button>
            {logo && (
              <Button variant="ghost" icon={Trash2} busy={remove.isPending} onClick={() => remove.mutate()}>
                {t("branding.logo.remove")}
              </Button>
            )}
          </div>
        </div>
        <p className="text-[12px] text-muted">{t("branding.logo.formats")}</p>
      </div>
    </Card>
  );
}

/** Settings → Branding: logo, name, colour and the mascot, with a preview of what they make. */
export function BrandingPage() {
  const { t } = useT();
  const refresh = useRefreshBrand();
  const query = useQuery({ queryKey: ["admin", "settings"], queryFn: () => api<SettingsView>("/api/admin/settings") });

  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;

  return (
    <div className="flex flex-col gap-5">
      <LogoCard />
      <Section
        title={t("branding.look.title")}
        intro={t("branding.look.intro")}
        view={query.data}
        keys={["brand.name", "brand.color", "brand.mascot"]}
        onSaved={refresh}
      >
        {(form) => {
          const color = (form.value("brand.color") as string | null) || null;
          return (
            <>
              <NameField form={form} />
              <ColorField form={form} />
              <ToggleField
                form={form}
                settingKey="brand.mascot"
                label={t("branding.mascot.label")}
                hint={t("branding.mascot.hint")}
              />
              <Preview
                color={color}
                name={String(form.value("brand.name") ?? "")}
                mascot={form.value("brand.mascot") !== false}
              />
            </>
          );
        }}
      </Section>
    </div>
  );
}
