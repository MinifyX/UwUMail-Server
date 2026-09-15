import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Eye, EyeOff } from "lucide-react";
import { useState, type FormEvent, type ReactNode } from "react";
import { NyuScene } from "@/components/nyu/scenes";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button, IconButton } from "@/components/ui/Button";
import { EmptyState } from "@/components/ui/EmptyState";
import { Field, TextInput } from "@/components/ui/Field";
import { Wordmark } from "@/components/ui/Logo";
import { useT } from "@/i18n";
import { api, ApiError, setCsrfToken, type PasswordLinkInfo, type Session } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDateTime } from "@/lib/format";
import { navigate } from "@/lib/router";
import { usePrefs } from "@/state/prefs";

const MIN_CHARS = 10;

/** Where invited people and people with a reset link choose their password. Works logged in or out. */
export function PasswordPage({ token }: { token: string }) {
  const { t, i18n } = useT();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  const [password, setPassword] = useState("");
  const [repeat, setRepeat] = useState("");
  const [visible, setVisible] = useState(false);
  const [touched, setTouched] = useState(false);

  const link = useQuery({
    queryKey: ["password-link", token],
    queryFn: () => api<PasswordLinkInfo>(`/api/password-links/${encodeURIComponent(token)}`),
    staleTime: Infinity,
  });
  const choose = useMutation({
    mutationFn: () =>
      api<Session>(`/api/password-links/${encodeURIComponent(token)}`, { method: "POST", body: { password } }),
    onSuccess: (session) => {
      setCsrfToken(session.csrfToken);
      usePrefs.getState().apply(session.preferences);
      queryClient.setQueryData(["session"], session);
      navigate("/account", { replace: true });
    },
  });

  const wrapper = (content: ReactNode) => (
    <main className="flex min-h-screen items-center justify-center px-4 py-10">
      <div className="w-full max-w-[420px] animate-slide-up">
        <div className="mb-6 flex justify-center">
          <Wordmark className="text-2xl" />
        </div>
        {content}
      </div>
    </main>
  );

  if (link.isPending) return <Loading fullPage />;
  const invalid =
    (link.error instanceof ApiError && ["linkInvalid", "notFound"].includes(link.error.code)) ||
    (choose.error instanceof ApiError && choose.error.code === "linkInvalid");
  if (invalid) {
    return wrapper(
      <EmptyState
        scene="loadError"
        title={t("password.invalid.title")}
        body={t("password.invalid.body")}
        action={
          <Button variant="primary" onClick={() => navigate("/login", { replace: true })}>
            {t("password.invalid.login")}
          </Button>
        }
      />,
    );
  }
  if (link.isError) return <LoadError error={link.error} onRetry={() => void link.refetch()} />;

  const info = link.data;
  const missing = Math.max(0, MIN_CHARS - [...password].length);
  const mismatch = touched && repeat.length > 0 && repeat !== password;
  const submit = (event: FormEvent) => {
    event.preventDefault();
    setTouched(true);
    if (missing === 0 && repeat === password) choose.mutate();
  };
  const name = info.name || info.login.split("@")[0];

  return wrapper(
    <div className="rounded-[22px] border border-hairline bg-surface px-6 pt-4 pb-7 shadow-float sm:px-8">
      <NyuScene name={info.purpose === "invite" ? "welcome" : "pick"} className="mx-auto h-auto w-[180px]" />
      <h1 className="mt-1 text-center text-[20px] font-bold">
        {info.purpose === "invite" ? t("password.inviteTitle", { name }) : t("password.resetTitle")}
      </h1>
      <p className="mt-1 text-center text-[13px] text-muted">
        {t(info.purpose === "invite" ? "password.inviteBody" : "password.resetBody", { login: info.login })}
      </p>
      <form className="mt-5 flex flex-col gap-4" onSubmit={submit}>
        {/* Lets password managers save the new password for the right address. */}
        <input type="email" autoComplete="username" value={info.login} readOnly hidden />
        <Field
          label={t("password.password")}
          hint={missing > 0 && password ? t("password.missing", { count: missing }) : t("password.hint")}
        >
          {(id) => (
            <span className="relative block">
              <TextInput
                id={id}
                type={visible ? "text" : "password"}
                autoComplete="new-password"
                autoFocus
                required
                className="pr-12"
                value={password}
                onChange={(event) => setPassword(event.target.value)}
              />
              <IconButton
                size="sm"
                icon={visible ? EyeOff : Eye}
                label={visible ? t("common.hidePassword") : t("common.showPassword")}
                className="absolute top-1/2 right-1.5 -translate-y-1/2"
                onClick={() => setVisible((value) => !value)}
              />
            </span>
          )}
        </Field>
        <Field
          label={t("password.repeat")}
          error={mismatch ? t("password.mismatch") : choose.isError ? errorText(choose.error) : undefined}
        >
          {(id) => (
            <TextInput
              id={id}
              type={visible ? "text" : "password"}
              autoComplete="new-password"
              required
              value={repeat}
              onChange={(event) => setRepeat(event.target.value)}
              onBlur={() => setTouched(true)}
            />
          )}
        </Field>
        <Button
          type="submit"
          variant="primary"
          size="lg"
          busy={choose.isPending}
          disabled={missing > 0}
          className="mt-1"
        >
          {t("password.submit")}
        </Button>
        <p className="text-center text-[12px] text-faint">
          {t("password.validUntil", { date: formatDateTime(info.expiresAt, i18n.language) })}
        </p>
      </form>
    </div>,
  );
}
