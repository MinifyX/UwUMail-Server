# Plan: Paketerkennung, Spam bei Versandmails, Paketabfrage, Spam-Netz

Stand 16.11.2026. Entscheidungen von Lorin; **nicht releasen**, bis Lorin es freigibt
(weitere Beispiel-Mails kommen). Kein Deploy des Spam-Netz-Dienstes vor dem Release.

## Arbeitspakete und Branches

| Paket | Repo / Branch | Worktree |
| --- | --- | --- |
| A Erkennung + Spam bei Versandmails | Server `fix/shipment-detection` | Haupt-Checkout |
| B Paketabfrage bei Paketdiensten | Server `feat/parcel-lookup` | `wt/uwumail-parcel-lookup` |
| C Spam-Netz-Dienst | neues Repo `MinifyX/UwUSpamNet` (public, AGPL-3.0) | `~/projects/UwUSpamNet` |
| D Spam-Netz-Client im Server | Server `feat/spamnet` | `wt/uwumail-spamnet` |
| E Webmail + App | danach, gegen die Verträge unten | – |

Migrationen: A nimmt `0096`, B `0097`, D `0098` (je nach Bedarf, Nummern nicht doppelt
vergeben). Beim Zusammenführen auf A's Branch mergen.

## A Erkennung + Spam bei Versandmails

1. Erkennung: Datum mit Satzpunkt („06.11.2026.“), Status „ist fast bei dir/Ihnen“ → `inTransit`.
2. `shopTrackingUrl`: Link aus der Mail auf der Domain des Shops (Absender-Domain bzw. deren
   registrierbare Domain), nur bei authentifizierter Mail, `utm_*`/`oxm_*`/`mc_*`-Parameter
   entfernt, nie abgerufen. Neues JMAP-Feld `shopTrackingUrl` (string|null), Spalte
   `shop_tracking_url`.
3. Kein Paket aus Mail, deren Spam-Ergebnis nicht `Deliver` ist (also `Suspicious`, Junk,
   Reject) und die nicht authentifiziert ist, oder die eine Marken-Nachahmung-Regel
   (`BRAND_*`/`LOOKALIKE_*`) trifft, ohne authentifiziert zu sein.
4. Mail wird (von Hand oder Regel) nach Junk verschoben → Mail-/Modell-Events dieser Mail
   entfernen, Status neu abspielen (`replay`); Pakete ohne verbleibende Events und ohne
   `title`/`hidden`/Client-Event werden gelöscht (destroyed-Changes).
5. Neue Spam-Regeln (`spam/phishing.rs` bzw. eigenes `spam/parcels.rs`):
   - `PARCEL_FEE_UNAUTH`: nicht authentifizierte Versand-/Paketdienst-Mail mit Hinweis auf
     Gebühr, Zoll, Nachzahlung, Adresse bestätigen/aktualisieren, Zustellgebühr.
   - `PARCEL_BAD_NUMBER`: Sendungsnummer im Format eines Paketdienstes mit falscher
     Prüfziffer (DHL 20-stellig/JJD, UPS 1Z, GLS, DPD wo bekannt).
   - `PARCEL_FOREIGN_LINK`: Mail nennt Paketdienst, „Sendung verfolgen“-artiger Link führt
     auf Domain, die weder Shop (Absender) noch Paketdienst ist, Mail nicht authentifiziert.

## B Paketabfrage (Sendungsnummer → Paketdienst)

- Nie ein Link aus der Mail. Nur Sendungsnummer + Paketdienst an feste Endpunkte.
- Dienste: DHL (offizielle Shipment Tracking – Unified API, Header `DHL-API-Key`),
  UPS (OAuth Client-Credentials, Track API), FedEx (OAuth, Track API). DPD/Hermes/GLS
  wurden am 16.11.2026 wieder entfernt (inoffizielle Endpunkte tot); dort bleibt der Mail-Link.
- Admin-Settings: `shipments.lookup.enabled` (Bool, Default aus),
  `shipments.lookup.dhl_key` (Secret), `shipments.lookup.ups_client_id` (Text),
  `shipments.lookup.ups_client_secret` (Secret), `shipments.lookup.fedex_client_id` (Text),
  `shipments.lookup.fedex_client_secret` (Secret), (kein Schalter für inoffizielle Dienste mehr).
- Konto-Schalter wie die Paketerkennung (eigenes Konto kann ausschalten), Default an,
  wenn Admin an.
- Egress: neuer `Purpose::Parcels`, im Admin über den Proxy schaltbar wie die anderen.
- Takt: offene Pakete alle 2 h, `outForDelivery` alle 30 min; nur bis 30 Tage nach der
  letzten Mail; nie nach `delivered`/`returned`/`cancelled`. Je Dienst Ratenlimit
  (DHL: höchstens 1 Abfrage/5 s, 250/Tag; Kontingent sparsam teilen).
- Ergebnisse → Events mit `source = "carrier"` (Text = Status-Text des Dienstes,
  `location`), Status-Mapping auf `ShipmentStatus`, `expected` übernehmen. Gleiche Events
  nicht doppelt.
- JMAP:
  - Capability `urn:uwumail:jmap:shipments` bekommt `"lookupCarriers": ["dhl", …]`
    (leer = aus für dieses Konto).
  - Shipment bekommt `lookedUpAt` (UTCDate|null) und `lookupError` (string|null:
    `notFound`, `unavailable`, `rateLimited`).
  - Neue Methode `Shipment/refresh` `{accountId, ids}` → `{accountId, refreshed: [id],
    notRefreshed: {id: {type: "tooSoon"|"notSupported"|"unavailable"|"notFound"}}}`;
    höchstens 1× je 5 min je Paket; synchron mit Zeitlimit (10 s), Änderungen kommen über
    den normalen State.
- Gegenprüfung verdächtiger Mails: Mail mit Sendungsnummer, Spam-Ergebnis `Suspicious`
  oder Marken-Nachahmung, nicht authentifiziert, Abfrage an → Paketdienst fragen; sagt er
  „unbekannt“, Spam-Finding `PARCEL_NUMBER_UNKNOWN` ins Spam-Log und Mail nach Junk
  verschieben (nur wenn der Dienst eindeutig „unbekannt“ sagt, nie bei Fehler/Timeout).
- Portal: Admin-Karte „Paketabfrage“ (Schalter, Keys, Links zu den Entwicklerportalen),
  Kontoschalter, einmalige Hinweiskarte „Neu: Spam-Netz und Paketabfrage“ (mit D teilen).
- Doku: `docs/jmap-shipments.md` (Satz „tracking pages are never fetched“ anpassen:
  Links aus Mails weiter nie, Paketdienst-Abfragen nur per Nummer und opt-in),
  Datenschutz-Hinweis für Betreiber.

## C/D Spam-Netz (UwUSpamNet)

Offen für beliebige Mailserver (nicht nur UwUMail), Teilnahme nur mit Registrierung +
Key. Dienst unter `https://spamnet.minifyx.de`. Repo `MinifyX/UwUSpamNet`.

### Was geteilt wird

Nur Merkmale, nie Mailinhalte, nie Empfängeradressen, nie Bayes-Wörter:
- `digest`: Text-Fingerabdrücke (Spezifikation unten, nicht umkehrbar)
- `domain`: registrierbare Domains aus Links
- `ip`: einliefernde IP (IPv4 /32, IPv6 /64)
- `from`: registrierbare Absender-Domain (RFC5322.From)

Gemeldet wird `spam` (Person verschiebt nach Junk, oder Filter mit Punkten ≥ Reject-Grenze)
und `ham` (Person holt aus Junk zurück / „kein Spam“).

### Protokoll v1 (HTTPS, JSON)

- `POST /v1/register` `{ "name": "mail.example.com", "software": "UwUMail 0.38.0",
  "contact": "postmaster@example.com" }` → `201 { "serverId": "s_…", "key": "usn_…" }`.
  Key nur einmal sichtbar, gespeichert als Hash. Ratenlimit je IP (5/Tag). `contact`
  optional.
- Auth: `Authorization: Bearer usn_…`.
- `POST /v1/reports` `{ "reports": [ { "verdict": "spam"|"ham", "reason":
  "user"|"filter", "digests": ["d1:…"], "domains": ["…"], "ip": "203.0.113.7",
  "from": "example.net" } ] }` → `202 { "accepted": n }`. Höchstens 100 Meldungen,
  je Meldung höchstens 8 digests, 20 domains. Ratenlimit je Server (z. B. 5000/Tag).
- `GET /v1/list?since=<cursor>` → `{ "cursor": "…", "full": bool, "entries": [ { "kind":
  "digest"|"domain"|"ip"|"from", "value": "…", "score": 1.0–4.0, "expires": "RFC3339" } ],
  "removed": [ { "kind", "value" } ] }`. Ohne `since` die volle Liste. Clients holen
  alle 15 min.
- `GET /` kurze Infoseite + Datenschutzhinweis; `GET /v1/health`.
- Fehler: `{ "error": "…" }` mit passenden Statuscodes (401, 413, 429).

### Listen-Regeln im Dienst (gegen Vergiftung)

- Eintrag gelistet, wenn ≥ 3 unterschiedliche Server (unterschiedlich auch nach
  Registrierungs-Netz /24 bzw. /48) ihn in 14 Tagen als `spam` gemeldet haben und
  spam-Meldungen ≥ 3× ham-Meldungen sind.
- `score` steigt mit der Zahl der Server (3 → 1.0, ab 10 → 4.0); `reason=user` zählt
  doppelt wie `filter`.
- Verfällt 14 Tage nach der letzten spam-Meldung; ham-Meldungen nehmen schneller raus.
- Allowlist (Admin des Dienstes) für Domains/IPs, die nie gelistet werden (große Marken,
  Paketdienste, Freemail-Domains bei `from`).
- Admin-CLI: Server sperren (seine Meldungen fallen weg), Allowlist pflegen, Statistik.
- Speichert: Server (id, Name, Key-Hash, Registrierungs-Netz, Zeitpunkt, gesperrt),
  Meldungen aggregiert je (kind, value, server) mit Zählern und letzter Zeit; keine IPs
  der meldenden Server außer dem Registrierungs-Netz.

### Digest v1

Eingabe: der lesbare Text der Mail (text/plain, sonst aus HTML gewonnen).
1. Unicode NFKC, Kleinschreibung.
2. URLs, E-Mail-Adressen und alle Ziffernfolgen entfernen.
3. Alles außer Buchstaben durch ein Leerzeichen ersetzen, Leerraum zu einem Leerzeichen
   zusammenfassen, trimmen.
4. Ganzer Text ≥ 40 Zeichen → `d1:` + erste 32 Hex-Zeichen von SHA-256 des Textes.
5. Absätze (an Leerzeilen der Eingabe getrennt, je wie 1–3 normalisiert) ≥ 60 Zeichen:
   die bis zu 4 längsten → `p1:` + erste 32 Hex von SHA-256.
Testvektoren liegen im Repo UwUSpamNet (`docs/digest-v1.md`) und werden im Server-Client
mitgeprüft.

### Client im UwUMail Server (D)

- Admin-Settings: `spamnet.fetch` (Bool, Default aus), `spamnet.report` (Bool, Default aus),
  `spamnet.url` (Text, Default `https://spamnet.minifyx.de`), Key automatisch bei der
  ersten Nutzung registriert und als Secret `spamnet.key` gespeichert.
- Egress: neuer `Purpose::SpamNet` (über Proxy schaltbar).
- Findings: `SPAMNET_DIGEST`, `SPAMNET_DOMAIN`, `SPAMNET_IP`, `SPAMNET_FROM` mit den
  Punkten aus der Liste (Summe gedeckelt bei 6).
- Meldet: Verschieben nach Junk / aus Junk (wo Bayes heute lernt), Filter-Reject.
  Meldungen gebündelt, Warteschlange in der DB, bei Fehler später erneut.
- Nie melden: Mails aus eigenen Domains/Konten, authentifizierte Mails von Kontakten.
- Portal: Admin-Karte „Spam-Netz“ (Abrufen/Melden getrennt, Erklärung, was geteilt wird,
  Status letzter Abruf/Meldungen), Hinweiskarte (mit B).
- Doku: `docs/spam.md` Abschnitt Spam-Netz, Textbaustein für die Datenschutzerklärung der
  Betreiber.

## E Webmail + App (danach)

`shopTrackingUrl` („Beim Shop ansehen“), „Jetzt aktualisieren“ (`Shipment/refresh`, nur wenn
`lookupCarriers` den Dienst enthält), `lookedUpAt`/`lookupError` anzeigen, Kontoschalter
Paketabfrage in den Einstellungen.
