# UwUMail Server 0.14.0 – großer Roadmap-Schub

## Context

Die „Later“-Liste in `UwUMail-Server/docs/roadmap.md` soll jetzt (fast) komplett gebaut und in einem Rutsch
veröffentlicht werden, ohne dass Lorin selbst testet. Umzusetzen:

1. Umzugsassistent im Portal (IMAP-Import mit eigenem Passwort, bisher nur CLI mit Dovecot-Master-User)
2. Gruppen, geteilte Postfächer, maskierte Adressen
3. Eigene TLS-Reports an andere Domains senden (RFC 8460), DANE (RFC 7672)
4. OAuth2/OIDC-Anbieter für Mail-Apps; Portal-Login über externes OIDC oder LDAP
5. Ruhigere Admin-Ansicht, Admin-Alarme, Statistiken, Prometheus-Metriken
6. Wiederherstellung einzelner Postfächer im Portal; Backups nach S3 oder in einen Ordner
7. Web Push (Webmail) und UnifiedPush (Android-App) über JMAP `PushSubscription`

Aus der Roadmap raus: „Public test instance“, „snooze on the server“ (Entscheidung: gestrichen),
„Settings sync for the UwUMail apps“ (ist mit JMAP `UserSettings` schon erledigt, steht in Abschnitt 2).

Veröffentlichung: Server **0.14.0** (PR → main, Tag `v0.14.0` auf Merge-Commit → CI baut Image + GitHub-Release),
Webmail-PR → main + neuer `webmail.pin`, Client **0.5.0-beta.4** (Release-Commit auf main, Android/iOS-Workflows
abwarten, Tag `v0.5.0-beta.4`). PRs merge und Tags pushe ich selbst (so bestätigt).

Branch überall: `claude/peaceful-hamilton-3653br`.

## Konventionen (aus der Exploration)

- Migrationen: `crates/uwumail-store/src/migrations/NNNN_name.sql`, von Hand in `db.rs` `MIGRATIONS` eintragen,
  Tabellen-Assertion im Migrationstest ergänzen. Keine CHECK-Erweiterung per Rebuild → neue Spalten/Tabellen.
  Reservierte Nummern: 0040 migration_jobs, 0041 groups + shared_mailboxes, 0042 masked_addresses,
  0043 tls_rpt (Sessions/Versand), 0044 oauth (clients, grants, tokens) + external_identities,
  0045 stats_daily + alerts, 0046 push_subscriptions.
- Store: `self.read/self.write`, JMAP-sichtbare Änderungen `next_modseq` + `record_change` + `notify_change`,
  Tests mit `test_support::store()`.
- Portal-API: `routes/*.rs` + `.route()` in `crates/uwumail-web/src/lib.rs` `Web::router()`, `ApiError::Rule`,
  Admin-Änderungen per `routes::audit`. Frontend: Seite in `web/src/app/App.tsx` `page()`, Navigation
  `features/shell/PortalShell.tsx`, Typen `web/src/lib/api.ts`, Fehlercodes `lib/errors.ts`, Mock `dev/mockApi.ts`,
  Texte in allen 12 Dateien `web/src/i18n/locales/{de,en,fr,nl,ja,zh}/{neutral,playful}.json`
  (`locales.test.ts` erzwingt Vollständigkeit).
- Server-Einstellungen: `crates/uwumail-web/src/settings.rs` `SETTINGS` + live `crates/uwumail-server/src/settings.rs`;
  feature-eigene JSON-Einstellungen via `store.setting/set_setting` (wie `backup.settings`).
- Hintergrundarbeit: DB-Zeile + Worker in `uwumail-server` (Muster `fetch.rs` `run_fetchers`, gestartet in `serve.rs`),
  Frontend-Polling per `refetchInterval` (Muster `BackupsPage.tsx`).
- Krypto nur über das vorhandene `aws-lc-rs` (ECDH P-256, ECDSA, HKDF, AES-GCM, HMAC, RSA-Verify), HTTP über
  `hyper-rustls`/Egress wie Cloudflare/Fetch. Neue Abhängigkeit nur `ldap3` (rustls) für LDAP.
- Texte: Englisch in Code/Docs; UI de/en (+fr/nl/ja/zh), playful + neutral, Beispielnamen nur RFC-2606-Domains.

## Umsetzung (je Punkt eigene Commits, conventional commits)

### 1. Umzugsassistent (Portal)
- Migration 0040 `migration_jobs(account_id, host, port, login, password_sealed, state, folders_done, messages_done,
  messages_total, bytes_done, error, started_at, finished_at, last_run_at)`; Passwort mit `seal/unseal`
  (`crates/uwumail-store/src/fetch.rs`).
- Worker `crates/uwumail-server/src/migrate.rs` (neben `fetch.rs`): nutzt `import::imap::{Connection, Source, folders,
  mailbox_for}` / `copy_mail` mit `master_user: None`, Egress-Dialer + Public-Address-Check wie `fetch.rs`,
  Duplikate per `holds_message`, `QuotaExceeded` → Job pausiert mit Fehlercode, Zeitscheiben (`RUN_LIMIT`), Fortsetzung
  über `import_progress`. `copy_mail` bekommt dafür eine Variante mit strukturiertem Fortschritt statt `FnMut(&str)`.
- Portal: *Mein Konto → Umzug*: Adresse + Passwort → Server via `uwumail_smtp::autoconfig::discover`
  (Provider-Hinweise Gmail/Outlook/GMX/WEB.DE: App-Passwort, IMAP einschalten), Fortschritt, „Nochmal abgleichen“
  (holt Neues seit dem letzten Lauf), „Fertig, Zugangsdaten löschen“. Verweis auf Kalender/Kontakte-Umzug.
- Tests: Store-Tests für Job-Zustände; Integrationstest mit In-Process-IMAP-Server (unser eigener `uwumail-imap` als
  Quelle) kopiert Ordner + Flags, zweiter Lauf kopiert nur Neues.

### 2. Gruppen, geteilte Postfächer, maskierte Adressen
- **Gruppen** (Migration 0041): `groups(id, local_part, domain_id, name, who_may_send: anyone|members|domain,
  members_may_send_as)`, `group_members(group_id, account_id)`. `address_in_use` + Kollisionsprüfungen
  (`own.rs`, `directory.rs`, `delete_domain`) erweitern. `inbound.rs rcpt()` löst Gruppe zu Mitgliedern auf; jedes
  Mitglied bekommt die Mail über den normalen Personenweg (eigene Spam-Entscheidung, Sieve), Sendeberechtigung
  wird geprüft (5.7.1 sonst). Send-as über `extras::owns` + Identitäten. Admin-UI unter Domains → Gruppen, Mitglieder
  sehen ihre Gruppen in Mein Konto. JMAP `Principal` mit `type: "group"`.
- **Geteilte Postfächer** (0041): neue Spalte `accounts.shared_mailbox`, Tabelle `shared_mailbox_members(account_id,
  member_id, rights, may_send)`. Rechte ergeben sich aus Mitgliedschaft für *alle* Ordner (auch neue) – in `acl.rs`
  als zusätzliche Quelle neben `mailbox_acl`, `ACTIVE_PERSON`-Filter entsprechend angepasst. Erscheint per IMAP
  `Shared/<adresse>/…` und als JMAP-Shared-Account. Senden: Mitglieder mit `may_send` bekommen eine Identität der
  geteilten Adresse im eigenen Account (`extras::owns`), Kopie in „Gesendet“ des geteilten Postfachs. Anlegen im
  Admin (Personen → Neues geteiltes Postfach), Quota zählt beim geteilten Postfach.
- **Maskierte Adressen** (0042): `masked_addresses(id, account_id, local_part, domain_id, state
  pending|enabled|disabled|deleted, for_domain, description, url, created_by, created_at, last_message_at)`.
  Admin legt pro Domain fest, ob sie maskierte Adressen erlaubt. Zufallsnamen `wort.wort123`. `resolve`: enabled →
  Inbox, disabled → still in den Papierkorb, deleted → 550; `pending` wird nach 24 h ohne Mail gelöscht.
  JMAP `MaskedEmail/get|set` (Fastmail-Spec `https://www.fastmail.com/dev/maskedemail`) nach dem
  `senders.rs`-Muster, ID-Präfix neu, Push-Typ. Portal: Mein Konto → Maskierte Adressen. Antworten „als“ maskierte
  Adresse über `extras::owns`. Doku `docs/jmap-masked-email.md`, `docs/groups.md` (Gruppen + geteilte Postfächer).
- Tests: SMTP-Flow-Tests (Gruppe an 3 Mitglieder, members-only-Ablehnung, geteiltes Postfach per IMAP/JMAP,
  maskierte Adresse enabled/disabled/deleted), JMAP-Integrationstest `masked_email.rs`.

### 3. TLS-Reports senden + DANE
- `outbound.rs session()` liefert ein TLS-Ergebnis (policy type sts/tlsa/no-policy-found, result type nach RFC 8460,
  MX, IP). MTA-STS-Abruffehler in `mta_sts::policy_for` werden mitgezählt. Migration 0043 `tls_rpt_sessions`
  (Tageszähler) + `tls_rpt_sent`.
- Täglicher Job (in `serve.rs`-Housekeeping): `_smtp._tls.<domain>` TXT holen, Bericht per `mail-auth`
  `report::tlsrpt` bauen, gzip, per `mailto:` (Queue, DKIM-signiert, Absender `noreply-tls-reports@<host-domain>`)
  oder `https:` POST über Egress. Einstellung `reports.send_tls_reports` (an). Keine Berichte an die eigenen Domains.
- **DANE ausgehend:** hickory mit DNSSEC-Validierung (Feature `dnssec-aws-lc-rs`), TLSA-Abfrage `_25._tcp.<mx>` nur
  bei validierter Antwort und validiertem MX; vorhandene TLSA (Usage 2/3) → STARTTLS Pflicht + dritter
  `ServerCertVerifier` in `tls.rs` (DANE-EE: Schlüssel/Zertifikat-Hash, Name egal; DANE-TA: Kette bis TA).
  DANE hat Vorrang vor MTA-STS. Fehlschläge → deferred + TLS-RPT `tlsa-invalid`/`dnssec-invalid`.
- **DANE eingehend:** DNS-Check empfiehlt bei DNSSEC-signierter Zone `TLSA 3 1 1` (+ Hinweis), ACME-Erneuerung
  behält den Schlüssel, damit der Eintrag gültig bleibt; Prüfung ob der veröffentlichte TLSA zum aktuellen Zertifikat
  passt, rote Warnung vor Schlüsselwechsel.
- Tests: Unit-Tests für TLSA-Matching (Test-Zertifikate aus rcgen), Report-JSON gegen RFC-Beispiel, SMTP-Flow-Test mit
  gepinnten DNS-Antworten (DNS-Cache-Pins um TLSA erweitern).

### 4. OAuth2/OIDC-Anbieter + Login über OIDC/LDAP
- **Anbieter** (Migration 0044 `oauth_clients`, `oauth_grants`, `oauth_tokens`): Endpunkte
  `/.well-known/oauth-authorization-server` + `/.well-known/openid-configuration`, `/oauth/authorize` (Portal-Seite
  mit Einwilligung, nutzt vorhandenen Portal-Login inkl. 2FA/Passkey), `/oauth/token` (authorization_code + PKCE S256
  Pflicht, refresh_token mit Rotation), `/oauth/register` (RFC 7591 dynamisch, öffentliche Clients, Loopback- und
  App-Schema-Redirects), `/oauth/revoke`, `/oauth/jwks`, `/oauth/userinfo`; ID-Token ES256. Scopes `mail`, `smtp`,
  `dav` (= `AppScope`) + `openid email profile`. Tokens gehasht gespeichert, erscheinen in Mein Konto → Sicherheit
  wie App-Passwörter (widerrufbar).
- Tokens gelten als `Bearer` für JMAP/DAV (`auth.rs` `bearer()`), als SASL `OAUTHBEARER` + `XOAUTH2` bei IMAP,
  SMTP-Submission und ManageSieve (Capabilities/EHLO ergänzen), zentral `Store::authenticate_oauth`.
- **Login über OIDC:** Einstellungen `auth.oidc.*` (Issuer, Client-ID, Secret sealed, Button-Name, automatisch anlegen
  ja/nein, erlaubte Domains). Discovery, Code-Flow mit PKCE + nonce, ID-Token-Prüfung (RS256/ES256 über JWKS),
  Zuordnung per `sub` in `external_identities`, beim ersten Mal per verifizierter E-Mail. Lokale 2FA bleibt Pflicht,
  falls eingerichtet. Button auf der Login-Seite.
- **Login über LDAP:** Einstellungen `auth.ldap.*` (URL ldaps/StartTLS, Bind-DN-Vorlage oder Suche mit Service-Bind,
  Filter, Mail-Attribut, Admin-Gruppe optional, automatisch anlegen). Passwortprüfung für Portal und – solange
  Hauptpasswörter erlaubt sind – Mail-Protokolle in `authenticate`/`authenticate_mail` für Konten mit
  `auth_source = ldap`. Crate `ldap3` (rustls). „Verbindung testen“-Knopf im Admin.
- Doku `docs/oauth.md`, `docs/login-oidc-ldap.md`. Tests: kompletter Code-Flow mit PKCE per `oneshot`, OAUTHBEARER
  bei IMAP/SMTP, OIDC-Login gegen einen kleinen In-Process-Fake-IdP, LDAP gegen einen minimalen Fake-LDAP-Server
  (BER simple bind) im Test.

### 5. Ruhigere Admin-Ansicht, Alarme, Statistiken, Prometheus
- **Ruhige Ansicht:** Admin-Einstellung (UserSettings) „Einfach“ / „Alles“. Einfach = eine Ampel aus den
  Health-Areas, darunter nur die offenen Punkte mit „Was tun“, Personen/Domains/Backups als Schnellzugriffe;
  restliche Navigation eingeklappt unter „Mehr“. Umschalter oben auf der Admin-Startseite.
- **Statistiken** (Migration 0045 `stats_daily(day, key, value)`): Zähler für Mail rein/raus/zugestellt/zurückgestellt/
  gescheitert, Spam (Junk/abgelehnt/Greylist), Viren, Logins fehlgeschlagen; In-Memory-Zähler, minütlich
  in die Tagestabelle geschrieben. Admin → Server → Statistik mit 30-Tage-Diagrammen (einfache SVG-Balken, dataviz-Regeln).
- **Alarme** (0045 `alerts`): bei Health-Verschlechterung (rot/gelb), Queue hängt, Speicher knapp, Zertifikat läuft
  ab, Backup gescheitert, TLS-/DMARC-Probleme: Eintrag in der Alarmliste + Mail an Admins (einstellbar pro Admin,
  gedrosselt, „wieder gut“-Mail). Texte in de/en, beide Töne.
- **Prometheus:** `GET /metrics` (Text-Format, handgeschrieben), aus per Default, Einstellung `metrics.token`
  (Bearer) und optional erlaubte Netze. Werte: Konten, Domains, Speicher, Queue, Zähler aus Statistik, Health-Level
  pro Area, Uptime, Build-Info. Doku `docs/metrics.md`.

### 6. Backups nach S3/Ordner + Wiederherstellung einzelner Postfächer
- `storage.rs`: `Storage::S3(S3)` neu (SigV4 per aws-lc-rs HMAC, Path- und Virtual-Host-Style, Endpunkt frei für
  MinIO/Backblaze/Hetzner/Wasabi, ListObjectsV2 mit Paging, PUT/GET/DELETE über hyper-rustls + Egress-Regeln),
  `Storage::Local` für einen eingehängten Ordner. `Target` wird ein getaggtes Enum, alte gespeicherte JSON ohne Tag
  wird weiter als SFTP gelesen. `Backups::open/look_at`, Routen `routes/backups.rs`, `BackupsPage.tsx` SettingsCard
  (Ziel-Auswahl SFTP / S3 / Ordner), CLI-Restore akzeptiert `s3://…` und Pfade.
- **Einzelnes Postfach wiederherstellen:** Admin wählt Snapshot → Person → Ordner (oder alle). Hintergrundjob:
  nur die Datenbank des Snapshots in ein Temp-Verzeichnis bauen, read-only mit rusqlite lesen, passende Mails per
  Blob-Hash holen, prüfen, mit `store.ingest` in einen neuen Ordner „Wiederhergestellt <Datum>“ (Ordnerstruktur
  darunter) einspielen, Duplikate überspringen; Fortschritt wie `Fetching`, blockiert parallel laufende Backups.
  Auch als CLI `uwumail-server backup restore-mailbox`.
- Tests: S3 gegen einen In-Process-Fake-S3 (hyper), SigV4 gegen AWS-Testvektor, Local-Ziel, Round-Trip Backup →
  Postfach löschen → einzelnes Postfach wiederherstellen.

### 7. Web Push / UnifiedPush
- **Server** (Migration 0046 `push_subscriptions`): JMAP `PushSubscription/get|set` (RFC 8620 §7.2): Verifizierung
  per `PushVerification`, `types`, `expires` (max. 7 Tage, verlängerbar), Verschlüsselung RFC 8291 (aes128gcm,
  ECDH P-256 + HKDF über aws-lc-rs, Test gegen RFC-8291-Anhang-A-Vektor), VAPID RFC 8292 (Server-Schlüssel in
  Settings, `vapid` in Session-Capability `urn:ietf:params:jmap:webpush-vapid`). Versand eines `StateChange` bei
  Änderungen, gebündelt/entprellt, über Egress (nur öffentliche HTTPS-Ziele), 404/410 → Abo löschen.
  Doku `docs/jmap-push.md`.
- **Webmail** (`UwUMail-Webmail`): Service Worker, Einstellung „Benachrichtigungen bei geschlossenem Tab“,
  Registrierung per `PushSubscription/set`, Verifizierung, Benachrichtigung mit Absender/Betreff (per JMAP
  nachgeladen mit Session-Cookie), Klick öffnet die Mail. Tests (vitest) für Registrierungslogik. Danach
  `webmail.pin` im Server auf den Merge-Commit.
- **Android-App** (`UwUMail-Client`): `org.unifiedpush.android:connector` in `build.gradle.kts`, Receiver-Klasse
  in Kotlin (`UnifiedPushReceiver.kt`), neue Engine-Funktion in `crates/uwumail-core/src/jmap.rs`
  (`PushSubscription/set` + Verifizierung) für JMAP-Konten, die die Capability anbieten; bei einer Push-Nachricht
  weckt die App die Engine zum Sync. Einstellung: UnifiedPush wenn ein Verteiler installiert ist, sonst weiter
  Vordergrund-Dienst. Release-Notes `release-notes/0.5.0-beta.4.json` (de/en), Versionen laut
  `release-notes/README.md`, Client-Roadmap-Eintrag.

### 8. Roadmap/Doku
- `docs/roadmap.md`: „Public test instance“ und „Settings sync …, snooze on the server“ entfernen, alle umgesetzten
  „Later“-Punkte abhaken (mit Doku-Verweisen), Web Push / UnifiedPush abhaken.
- `docs/vision.md`: Satz „A calmer view … may come back later“ und „Much of this is still on the roadmap“ anpassen.
- `CHANGELOG.md` Abschnitt 0.14.0 im gewohnten Stil (fette Einleitungen, Migrationen 0040–0046, **Updating.**
  inkl. Webmail-Pin), README-Feature-Liste falls betroffen.

## Reihenfolge

Server-Store/Migrationen zuerst → 6, 1, 2, 3, 5, 4, 7 (Server) → Portal-UI je Feature direkt mit → Webmail →
Client → Roadmap/Changelog → Release. Nach jedem Feature: `cargo fmt`, `clippy -D warnings`, betroffene
Crate-Tests, `pnpm` Checks im `web/`.

## Verifikation

- Server lokal komplett wie CI: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D
  warnings`, `cargo test --workspace --locked`, `shellcheck`/`bash deploy/tests/helpers.sh`; `web/`:
  `pnpm install --frozen-lockfile && pnpm format:check && pnpm typecheck && pnpm lint && pnpm test && pnpm build`.
- Laufender Server: Release-Build mit eingebetteter Webmail starten (`dev/`-Stack bzw. `/run`), per Playwright
  (Chromium vorhanden) Portal-Seiten durchklicken (Umzug, Gruppen, Maskierte Adressen, OAuth-Einwilligung,
  Backups-S3 gegen Fake, Statistik, ruhige Ansicht), `curl /metrics`, `/.well-known/openid-configuration`,
  `dev/client-compat.sh`, `dev/smoke.mjs`.
- Webmail: `pnpm format:check typecheck lint test build audit`, Push-Registrierung gegen lokalen Server mit einem
  Test-Push-Endpunkt (Entschlüsselung im Test prüfen).
- Client: `cargo fmt/clippy/test` für `uwumail-core`, `pnpm` Checks, Android-Build über den `android.yml`-Workflow
  (Emulator-Smoke-Test) vor dem Tag – lokal gibt es kein Android-SDK.
- Release: Server-PR → CI grün → Merge → Tag `v0.14.0` auf Merge-Commit → CI-Jobs `image` + `release` grün,
  GitHub-Release und `ghcr.io/minifyx/uwumail-server:0.14.0` prüfen. Webmail-PR → CI grün → Merge vorher (Pin).
  Client: PR → Merge → Android/iOS-Workflows grün → Tag `v0.5.0-beta.4` → `release.yml` grün.
