import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import clsx from "clsx";
import { Image, ImageUp, Trash2, UserRound } from "lucide-react";
import { useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Dialog } from "@/components/ui/Dialog";
import { Field, Select, Toggle } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, type DomainLogoState, type PictureFile, type PictureState, type PictureVisibility } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";

/** The side of the square the browser sends; the server scales to at most 512 anyway. */
const OUTPUT = 512;
/** The side of the square on screen while cropping. */
const VIEW = 256;
const MAX_ZOOM = 4;

/** Where the picture sits in the square: how much bigger than "just covers it", and its offset. */
interface Placement {
  zoom: number;
  x: number;
  y: number;
}

/** Keeps the picture covering the whole square, whatever the zoom and the dragging did. */
function clamp(placement: Placement, width: number, height: number): Placement {
  const scale = (VIEW / Math.min(width, height)) * placement.zoom;
  const maxX = Math.max(0, (width * scale - VIEW) / 2);
  const maxY = Math.max(0, (height * scale - VIEW) / 2);
  return {
    zoom: placement.zoom,
    x: Math.min(maxX, Math.max(-maxX, placement.x)),
    y: Math.min(maxY, Math.max(-maxY, placement.y)),
  };
}

/** Draws what the square shows into a canvas of `size` and hands it back as a PNG. */
function render(image: HTMLImageElement, placement: Placement, size: number): Promise<Blob | null> {
  const canvas = document.createElement("canvas");
  canvas.width = size;
  canvas.height = size;
  const context = canvas.getContext("2d");
  if (!context) return Promise.resolve(null);
  const factor = size / VIEW;
  const scale = (VIEW / Math.min(image.naturalWidth, image.naturalHeight)) * placement.zoom * factor;
  const width = image.naturalWidth * scale;
  const height = image.naturalHeight * scale;
  context.imageSmoothingQuality = "high";
  context.drawImage(
    image,
    size / 2 - width / 2 + placement.x * factor,
    size / 2 - height / 2 + placement.y * factor,
    width,
    height,
  );
  return new Promise((resolve) => canvas.toBlob(resolve, "image/png"));
}

/** A square cut of the chosen file: drag to move it, the slider to zoom. */
function CropDialog({
  source,
  round,
  busy,
  onClose,
  onDone,
}: {
  /** An object URL of the chosen file. */
  source: string;
  round: boolean;
  busy: boolean;
  onClose: () => void;
  onDone: (blob: Blob) => void;
}) {
  const { t } = useT();
  const [broken, setBroken] = useState(false);
  const image = useRef<HTMLImageElement>(null);
  const [size, setSize] = useState<{ width: number; height: number } | null>(null);
  const [placement, setPlacement] = useState<Placement>({ zoom: 1, x: 0, y: 0 });
  const drag = useRef<{ x: number; y: number; start: Placement } | null>(null);

  const place = (next: Placement) => size && setPlacement(clamp(next, size.width, size.height));
  const scale = size ? (VIEW / Math.min(size.width, size.height)) * placement.zoom : 1;

  const down = (event: ReactPointerEvent<HTMLDivElement>) => {
    event.currentTarget.setPointerCapture(event.pointerId);
    drag.current = { x: event.clientX, y: event.clientY, start: placement };
  };
  const move = (event: ReactPointerEvent<HTMLDivElement>) => {
    const from = drag.current;
    if (!from) return;
    place({ ...from.start, x: from.start.x + event.clientX - from.x, y: from.start.y + event.clientY - from.y });
  };

  return (
    <Dialog open onClose={onClose} title={t("pictures.crop.title")} closeOnOutsideClick={false}>
      <div className="flex flex-col items-center gap-4 px-6 pt-1 pb-6">
        <p className="self-start text-[13px] text-muted">{t("pictures.crop.intro")}</p>
        {broken ? (
          <p role="alert" className="text-[13px] text-danger">
            {t("errors.codes.pictureType")}
          </p>
        ) : (
          <div
            className={clsx(
              "relative shrink-0 cursor-grab touch-none overflow-hidden bg-canvas select-none active:cursor-grabbing",
              round ? "rounded-full" : "rounded-card",
            )}
            style={{ width: VIEW, height: VIEW }}
            onPointerDown={down}
            onPointerMove={move}
            onPointerUp={() => (drag.current = null)}
            onPointerCancel={() => (drag.current = null)}
          >
            <img
              ref={image}
              src={source}
              alt=""
              draggable={false}
              onLoad={(event) =>
                setSize({ width: event.currentTarget.naturalWidth, height: event.currentTarget.naturalHeight })
              }
              onError={() => setBroken(true)}
              className="pointer-events-none absolute top-1/2 left-1/2 max-w-none"
              style={
                size
                  ? {
                      width: size.width * scale,
                      height: size.height * scale,
                      transform: `translate(calc(-50% + ${placement.x}px), calc(-50% + ${placement.y}px))`,
                    }
                  : { visibility: "hidden" }
              }
            />
          </div>
        )}
        <label className="flex w-full max-w-64 flex-col gap-1 text-[13px] font-semibold text-muted">
          {t("pictures.crop.zoom")}
          <input
            type="range"
            min={1}
            max={MAX_ZOOM}
            step={0.01}
            value={placement.zoom}
            disabled={!size}
            onChange={(event) => place({ ...placement, zoom: Number(event.target.value) })}
            className="accent-pink"
          />
        </label>
        <div className="flex justify-end gap-2 self-stretch">
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button
            variant="primary"
            busy={busy}
            disabled={!size || broken}
            onClick={() => {
              if (!image.current) return;
              void render(image.current, placement, OUTPUT).then((blob) => {
                if (blob) onDone(blob);
                else toast(t("errors.codes.pictureType"), "error");
              });
            }}
          >
            {t("pictures.crop.save")}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}

/** The picture as it is now, round for people and groups, square for logos. */
function Current({ picture, round, label }: { picture: PictureFile | null; round: boolean; label: string }) {
  return (
    <span
      className={clsx(
        "flex size-20 shrink-0 items-center justify-center overflow-hidden border border-hairline bg-canvas text-faint",
        round ? "rounded-full" : "rounded-card",
      )}
    >
      {picture ? (
        <img src={picture.url} alt={label} className="size-full object-cover" />
      ) : round ? (
        <UserRound className="size-9" aria-hidden />
      ) : (
        <Image className="size-9" aria-hidden />
      )}
    </span>
  );
}

/** Choose a file, crop it, upload it; or remove what is there. */
function PictureButtons({
  endpoint,
  picture,
  round,
  onChanged,
}: {
  endpoint: string;
  picture: PictureFile | null;
  round: boolean;
  onChanged: (state: unknown) => void;
}) {
  const { t } = useT();
  const errorText = useErrorText();
  const input = useRef<HTMLInputElement>(null);
  // The chosen file as an object URL, made when it is chosen and let go when the dialog closes.
  const [chosen, setChosen] = useState<string | null>(null);
  const close = () => {
    if (chosen) URL.revokeObjectURL(chosen);
    setChosen(null);
  };
  const upload = useMutation({
    mutationFn: (blob: Blob) => api<unknown>(endpoint, { method: "PUT", body: blob }),
    onSuccess: (state) => {
      onChanged(state);
      close();
      toast(t("pictures.saved"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const remove = useMutation({
    mutationFn: () => api<unknown>(endpoint, { method: "DELETE" }),
    onSuccess: (state) => {
      onChanged(state);
      toast(t("pictures.removed"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });

  return (
    <div className="flex flex-wrap gap-2">
      <input
        ref={input}
        type="file"
        accept="image/png,image/jpeg,image/webp,image/gif"
        className="hidden"
        onChange={(event) => {
          const file = event.target.files?.[0];
          event.target.value = "";
          if (file) setChosen(URL.createObjectURL(file));
        }}
      />
      <Button icon={ImageUp} onClick={() => input.current?.click()}>
        {t(picture ? "pictures.replace" : "pictures.upload")}
      </Button>
      {picture && (
        <Button variant="ghost" icon={Trash2} busy={remove.isPending} onClick={() => remove.mutate()}>
          {t("pictures.remove")}
        </Button>
      )}
      {chosen && (
        <CropDialog
          source={chosen}
          round={round}
          busy={upload.isPending}
          onClose={close}
          onDone={(blob) => upload.mutate(blob)}
        />
      )}
    </div>
  );
}

const VISIBILITIES: PictureVisibility[] = ["off", "server", "public"];

/**
 * A picture with who may see it: one's own (`face` adds the switch for the Face header), or a
 * service's, shared mailbox's or group's for admins. `endpoint` answers with a {@link PictureState}.
 */
export function PictureCard({
  endpoint,
  title,
  intro,
  face = false,
  bare = false,
  className,
}: {
  endpoint: string;
  title: string;
  intro: string;
  face?: boolean;
  /** Without a card around it, for inside a dialog. */
  bare?: boolean;
  className?: string;
}) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const key = ["picture", endpoint];
  const query = useQuery({ queryKey: key, queryFn: () => api<PictureState>(endpoint) });
  const onChanged = (state: unknown) => queryClient.setQueryData(key, state);
  const change = useMutation({
    mutationFn: (body: { visibility?: PictureVisibility; sendFace?: boolean }) =>
      api<PictureState>(endpoint, { method: "PATCH", body }),
    onSuccess: onChanged,
    onError: (error) => toast(errorText(error), "error"),
  });

  const content = () => {
    if (query.isPending) return <Loading />;
    if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
    const state = query.data;
    const isPublic = state.visibility === "public";
    return (
      <div className="flex flex-col gap-4">
        <p className="-mt-1 text-[13px] text-muted">{intro}</p>
        <div className="flex flex-wrap items-center gap-4">
          <Current picture={state.picture} round label={title} />
          <PictureButtons endpoint={endpoint} picture={state.picture} round onChanged={onChanged} />
        </div>
        <p className="text-[12px] text-muted">{t("pictures.formats")}</p>
        <Field
          label={t("pictures.visibility.label")}
          hint={
            state.mayBePublic
              ? t(`pictures.visibility.${state.visibility}Hint`)
              : `${t(`pictures.visibility.${state.visibility}Hint`)} ${t("pictures.visibility.publicForbidden")}`
          }
        >
          {(id) => (
            <Select
              id={id}
              value={state.visibility}
              disabled={change.isPending}
              onChange={(event) => change.mutate({ visibility: event.target.value as PictureVisibility })}
            >
              {VISIBILITIES.map((value) => (
                <option key={value} value={value} disabled={value === "public" && !state.mayBePublic}>
                  {t(`pictures.visibility.${value}`)}
                </option>
              ))}
            </Select>
          )}
        </Field>
        {face && (
          <div className="border-t border-hairline pt-4">
            <Toggle
              checked={Boolean(state.sendFace)}
              disabled={change.isPending || !isPublic || !state.mayBePublic}
              onChange={(on) => change.mutate({ sendFace: on })}
              label={t("pictures.face.label")}
              description={
                <>
                  {t("pictures.face.explain")}{" "}
                  {!isPublic || !state.mayBePublic ? t("pictures.face.needsPublic") : t("pictures.face.privacy")}
                </>
              }
            />
          </div>
        )}
      </div>
    );
  };

  if (bare) {
    return (
      <section className={clsx("flex flex-col gap-2", className)}>
        <h3 className="text-sm font-bold">{title}</h3>
        {content()}
      </section>
    );
  }
  return (
    <Card title={title} className={className}>
      {content()}
    </Card>
  );
}

/** A domain's logo, which stands in for its addresses without a picture, and the public switch. */
export function DomainLogoCard({ domain }: { domain: string }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const endpoint = `/api/admin/domains/${encodeURIComponent(domain)}/logo`;
  const key = ["picture", endpoint];
  const query = useQuery({ queryKey: key, queryFn: () => api<DomainLogoState>(endpoint) });
  const onChanged = (state: unknown) => queryClient.setQueryData(key, state);
  const setPublic = useMutation({
    mutationFn: (allowed: boolean) =>
      api<DomainLogoState>(`/api/admin/domains/${encodeURIComponent(domain)}/public-pictures`, {
        method: "PUT",
        body: { allowed },
      }),
    onSuccess: (state) => {
      onChanged(state);
      // The DNS check recommends the Libravatar record only while public pictures are allowed.
      void queryClient.invalidateQueries({ queryKey: ["admin", "domains", domain] });
    },
    onError: (error) => toast(errorText(error), "error"),
  });

  return (
    <Card title={t("pictures.domain.title")}>
      {query.isPending ? (
        <Loading />
      ) : query.isError ? (
        <LoadError error={query.error} onRetry={() => void query.refetch()} />
      ) : (
        <div className="flex flex-col gap-4">
          <p className="-mt-1 text-[13px] text-muted">{t("pictures.domain.intro")}</p>
          <div className="flex flex-wrap items-center gap-4">
            <Current picture={query.data.picture} round={false} label={t("pictures.domain.title")} />
            <PictureButtons endpoint={endpoint} picture={query.data.picture} round={false} onChanged={onChanged} />
          </div>
          <div className="border-t border-hairline pt-4">
            <Toggle
              checked={query.data.publicPictures}
              disabled={setPublic.isPending}
              onChange={(on) => setPublic.mutate(on)}
              label={t("pictures.domain.public")}
              description={
                query.data.serverAllowsPublic ? t("pictures.domain.publicHint") : t("pictures.domain.serverForbids")
              }
            />
          </div>
        </div>
      )}
    </Card>
  );
}

/** Server → Settings: whether pictures may be public anywhere on this server. */
export function PublicPicturesCard() {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const query = useQuery({
    queryKey: ["admin", "pictures"],
    queryFn: () => api<{ publicAllowed: boolean }>("/api/admin/pictures"),
  });
  const save = useMutation({
    mutationFn: (allowed: boolean) =>
      api<{ publicAllowed: boolean }>("/api/admin/pictures", { method: "PUT", body: { allowed } }),
    onSuccess: (state) => {
      queryClient.setQueryData(["admin", "pictures"], state);
      toast(t("pictures.server.saved"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });

  return (
    <Card title={t("pictures.server.title")}>
      {query.isPending ? (
        <Loading />
      ) : query.isError ? (
        <LoadError error={query.error} onRetry={() => void query.refetch()} />
      ) : (
        <Toggle
          checked={query.data.publicAllowed}
          disabled={save.isPending}
          onChange={(on) => save.mutate(on)}
          label={t("pictures.server.public")}
          description={t("pictures.server.publicHint")}
        />
      )}
    </Card>
  );
}
