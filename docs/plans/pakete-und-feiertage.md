# Plan: Pakete (Shipments) und eingebaute Feiertags-/KW-Kalender

Arbeitsauftrag für den nächsten Feature-Release (Vorschlag **0.36.0**). Geschrieben als Übergabe an
einen Agenten, der das umsetzt, testet und released. Die Entscheidungen unten sind mit Lorin
abgestimmt; was offen ist, steht unter [Offene Punkte](#offene-punkte).

Teile:

1. [Paket-Erkennung](#1-paket-erkennung-crate-uwumail-labels) im Crate `uwumail-labels`
2. [Pakete speichern und zusammenführen](#2-pakete-speichern-und-zusammenführen-uwumail-store)
3. [Pipeline: Zustellung, KI, Rückblick](#3-pipeline-zustellung-ki-rückblick)
4. [JMAP-Erweiterung `urn:uwumail:jmap:shipments`](#4-jmap-erweiterung-urnuwumailjmapshipments)
5. [Eingebaute Feiertags- und Kalenderwochen-Kalender](#5-eingebaute-feiertags--und-kalenderwochen-kalender)
6. [Portal, Webmail, App](#6-portal-webmail-app)
7. [Release](#7-release)

Ziel: Der Server weiß, welche Pakete gerade zu einer Person unterwegs sind: Paketdienst,
Sendungsnummer, Status, Liefertag, Shop, Tracking-Link und Verlauf. Webmail und Apps zeigen das als
Pakete-Ansicht (ähnlich wie Apple Wallet). Externe Programme wie Home Assistant lesen und
aktualisieren es über JMAP. Erster Markt ist **Deutschland**. Alles, was hier „Regel“ heißt, muss
ohne KI funktionieren. Die KI ist nur Rückfallebene.

**Der Server fragt die Paketdienste selbst nicht ab.** Den Live-Status holen Clients (zuerst Home
Assistant) und schreiben ihn über `Shipment/set` zurück (siehe 4). So braucht der Server keine
API-Keys und keine Scraper.

---

## 1. Paket-Erkennung (Crate `uwumail-labels`)

Heute liefert `Facts` schon `tracking: Option<(carrier, number)>` und `carrier`
(`detect.rs`: `own_tracking`, `carrier_in`, `tracking`). Das reicht fürs Label `shipping`, aber
nicht für ein Paket-Objekt.

Neu ist eine reine Funktion ohne I/O, damit die App sie byte-identisch übernehmen kann (siehe
Client-`CLAUDE.md`: `crates/uwumail-labels` ist eine Kopie):

```rust
pub fn shipment(mail: &Mail) -> Option<ShipmentFacts>

pub struct ShipmentFacts {
    pub carrier: Option<Carrier>,          // Dhl, DeutschePost, Dpd, Hermes, Gls, Ups, AmazonLogistics, Fedex, Other(String)
    pub numbers: Vec<String>,              // eine Mail kann mehrere Pakete nennen
    pub tracking_url: Option<String>,      // aus dem Mailtext, sonst aus Vorlage gebaut (s. u.)
    pub status: Option<ShipmentStatus>,
    pub expected: Option<DateWindow>,      // Tag, optional Uhrzeit-Fenster („zwischen 10:15 und 12:15“)
    pub merchant: Option<String>,          // Shop (Display-Name/Domain, „Amazon“, Shopify-Shopname)
    pub order_ref: Option<String>,         // Bestellnummer
    pub pickup: Option<Pickup>,            // Packstation/Filiale/Paketshop + Abholfrist
    pub confidence: f32,
}

pub enum ShipmentStatus { Ordered, Shipped, InTransit, OutForDelivery, Delivered,
                          ReadyForPickup, DeliveryFailed, Returned, Cancelled }
```

### Was erkannt werden muss (DE zuerst)

| Absender/Typ | Merkmale | Status-Signale (Beispiele) |
| --- | --- | --- |
| **DHL Paket** | `noreply@dhl.de`, `JJD…`, `00340434…`, 12- oder 20-stellig, Links `dhl.de/…/sendungsverfolgung…?piececode=` | „wird heute zugestellt“, „Zustellung voraussichtlich zwischen …“, „wurde zugestellt“, „liegt zur Abholung bereit“ (Packstation/Filiale + Frist), „Ablageort“, „Nachbar“ |
| **Deutsche Post** (Warensendung, Einschreiben) | `RR…DE`, `RT…DE`, `LX…DE`, `CA…DE` | Einschreiben „zugestellt“ |
| **DPD** | 14-stellig (oft `0…`), Links `tracking.dpd.de/status/…/parcel/<nr>`, `dpd.de/…parcelNumber=` | „Predict“-Mails mit 1-Stunden-Fenster, „heute“, „zugestellt“ |
| **Hermes** | 14- oder 16-stellig, `H…`-Nummern (20 Zeichen), Links `myhermes.de/empfangen/sendungsverfolgung/…` | „wird heute zugestellt“, „Zustellzeitfenster“, „im PaketShop“ |
| **GLS** | 11/12-stellig oder 8 Zeichen alphanumerisch, Links `gls-pakete.de/…match=` | FlexDeliveryService-Mails mit Fenster |
| **UPS** | `1Z` + 16 Zeichen, Links `ups.com/track?tracknum=` | „My Choice“: Zustelltag/-fenster, „Zugestellt“, „UPS Access Point“ |
| **Amazon** | `TBA…` (Amazon Logistics), Mails von `versandbestaetigung@amazon.de`, `bestellbestaetigung@`, `shipment-tracking@`, Fortschrittslinks `amazon.de/progress-tracker/…` | „Zustellung heute“, „Zugestellt“; Carrier kann auch DHL/Hermes sein (Nummer + Carrier aus Text) |
| **Shopify-Shops** | Header/Links `shopify.com`, `myshopify.com`, `/orders/…/authenticate`, Texte „Deine Bestellung ist unterwegs“, „Versandbestätigung“, Button „Bestellung verfolgen“/„Sendung verfolgen“ mit Carrier-Link | Status meist nur `Shipped`; Carrier und Nummer aus dem Tracking-Link |
| **Andere Shops** (Otto, Zalando, MediaMarkt, Ikea, Temu, AliExpress …) | Order-Wörter + Carrier-Link | Rückfall auf generischen Teil (Link → Carrier/Nummer) |

Grundsätze:

- **Lieber kein Paket als ein falsches.** Gleiches Leitmotiv wie bei den Labels. Eine Nummer allein
  (10–20 Ziffern) zählt nur zusammen mit Carrier und Versandwort (wie heute `tracking()`).
- **Links sind die beste Quelle.** Erst Tracking-Links auswerten (Host plus Query/Pfad pro Carrier),
  dann Text. Redirect-/Klick-Tracker (`click.`, `links.`, Safe Links) über die vorhandene Logik zur
  echten Adresse auflösen, aber **nicht aufrufen**.
- **Tracking-Link bauen**, wenn die Mail keinen hat, aus festen Vorlagen pro Carrier (öffentliche
  Sendungsverfolgung, Nummer URL-codiert).
- **Datum/Fenster** mit der vorhandenen Datumserkennung (`Facts` „dates/times“, siehe auch
  `docs/llm.md#dates`). Relative Angaben („heute“, „morgen“) beziehen sich auf das `Date` der Mail
  in der Zeitzone der Person.
- Auch die vorhandenen Funktionen `own_tracking` und `carrier_in` um DPD, Hermes, GLS und
  Deutsche Post erweitern. Davon profitiert auch das Label `shipping`. Danach `docs/labels.md`
  nachziehen.

### Tests

- Integrationstests in die vorhandene Test-Binary von `uwumail-labels` einhängen.
- **Pro Carrier und Status mindestens eine realistische deutsche Beispielmail** (erfunden, keine
  echten Namen, Adressen oder Nummern). Dazu englische Varianten für UPS und Amazon und drei
  Shopify-Shops mit unterschiedlichen Themes.
- Negativfälle: Werbung mit „kostenloser Versand“, Rechnung mit Paketnummer im Kleingedruckten,
  Newsletter von DHL, Retourenlabel-Mail (Status `Returned` nur, wenn die Retoure wirklich
  unterwegs ist).
- Korpus `tests/corpus/mails.jsonl` um die neuen Fälle erweitern. Die Messung
  (`docs/labels.md#measuring`) darf für `shipping` nicht schlechter werden.

---

## 2. Pakete speichern und zusammenführen (`uwumail-store`)

Neue Migration, zwei Tabellen (Namen nach vorhandenem Schema anpassen):

- `shipments`: `id`, `account_id`, `carrier`, `number` (nullable, solange nur eine
  Bestellbestätigung da ist), `tracking_url`, `merchant`, `order_ref`, `status`, `status_at`,
  `expected_from`, `expected_to`, `delivered_at`, `pickup_json`, `title` (vom Nutzer
  überschreibbar), `hidden`, `created_at`, `updated_at`. Eindeutig über
  `(account_id, carrier, number)`, wenn `number` gesetzt ist.
- `shipment_events`: `shipment_id`, `at`, `status`, `text`, `location`, `source`
  (`mail` | `ai` | `client`), `email_id` (nullable), `client_name` (nullable).

Regeln zum Zusammenführen:

1. Gleicher Carrier und gleiche Nummer ergeben dasselbe Paket.
2. Gleicher Shop und gleiche Bestellnummer ohne Nummer: Die Bestellbestätigung wird zum Paket,
   sobald die Versandmail mit Nummer kommt. Eine Bestellung mit mehreren Nummern ergibt mehrere
   Pakete mit derselben `order_ref`.
3. Amazon: Bestellnummer `\d{3}-\d{7}-\d{7}` ist der Schlüssel, bis eine Nummer da ist.
4. **Der Status geht nur vorwärts** (Reihenfolge wie im Enum), außer `DeliveryFailed`,
   `Returned` und `Cancelled`. Ein Event mit älterem `at` ändert den aktuellen Status nicht und
   landet nur im Verlauf.
5. Löscht die Person die Mail, bleibt das Paket bestehen (`email_id` wird `null`).

Aufräumen: Pakete mit `Delivered`/`Returned`/`Cancelled` sind nach **30 Tagen** weg,
unzugestellte ohne neues Event nach **60 Tagen**. Beide Fristen als Server-Konstanten. Pakete
gehören zu den Kontodaten, also auch in Backup und Wiederherstellung (und in einen Konto-Export, falls vorhanden).

---

## 3. Pipeline: Zustellung, KI, Rückblick

- **Bei der Zustellung** (dort, wo die Labels laufen, gleiches Zeitbudget, Fehler dürfen die
  Zustellung nie blockieren): Hat die Mail das Label `shipping` oder
  `Facts::shipment_evidence`, läuft `shipment(mail)`. Erst ab `confidence ≥ 0.8` wird angelegt
  bzw. aktualisiert. Junk wird ignoriert.
- **KI-Rückfall** (nur wenn die Person die KI-Labels an hat, über den vorhandenen Label-Worker in
  `uwumail-assist`): Ist die Mail `shipping`, aber die Regeln liefern keine Nummer oder keinen
  Status, fragt der Worker das Modell nach genau dem Schema von `ShipmentFacts` (JSON, wie bei
  `Assist/extractEvents`). Es gelten „Mail is data, not orders“ (`docs/llm.md`), Kosten- und
  Ratenlimits, und die Person muss KI erlaubt haben. Ergebnis mit `source: "ai"`. Eine
  KI-Nummer wird nur übernommen, wenn sie wörtlich im Mailtext steht.
- **Rückblick:** Beim ersten Einschalten der Funktion für ein Konto (und über `Shipment/rescan`)
  werden die Mails der letzten **14 Tage** (Parameter, max. 90) mit Label `shipping` oder
  Versand-Evidenz durchgesehen, älteste zuerst, damit der Status richtig vorwärts läuft. Läuft als
  Hintergrund-Job (vorhandene Job-Muster prüfen, z. B. `migration_jobs.rs`) und nicht in der Request-Zeit.
- **Einstellung pro Person:** `shipments` an/aus (Standard **an**), über die vorhandenen
  JMAP-Settings und im Portal unter „Mein Konto“. Aus heißt: nichts erkennen, nichts speichern,
  vorhandene Pakete löschen.

---

## 4. JMAP-Erweiterung `urn:uwumail:jmap:shipments`

Capability in `session.rs` (wie `BIRTHDAYS`), Doku als `docs/jmap-shipments.md` im Stil der anderen
`docs/jmap-*.md`. Neuer **Datentyp `Shipment`** im State/Push. Änderungen kommen über EventSource,
WebSocket und Web Push wie bei `Email`.

### Shipment-Objekt

```json
{
  "id": "s7",
  "carrier": "dhl",
  "number": "00340434000000000000",
  "trackingUrl": "https://…",
  "title": null,
  "merchant": "Beispielshop",
  "orderRef": "12345",
  "status": "outForDelivery",
  "statusAt": "2026-11-15T08:12:00Z",
  "expected": { "from": "2026-11-15T10:15:00+01:00", "to": "2026-11-15T12:15:00+01:00", "allDay": false },
  "deliveredAt": null,
  "pickup": null,
  "hidden": false,
  "emailIds": ["e42", "e57"],
  "events": [
    { "at": "2026-11-14T18:03:00Z", "status": "shipped", "text": "Sendung wurde an DHL übergeben", "location": null, "source": "mail", "emailId": "e42" },
    { "at": "2026-11-15T08:12:00Z", "status": "outForDelivery", "text": "Zustellung heute", "location": "Musterstadt", "source": "client", "clientName": "Home Assistant" }
  ],
  "created": "…", "updated": "…"
}
```

`pickup`: `{ "kind": "packstation" | "postOffice" | "parcelShop" | "accessPoint", "name": "…", "address": "…", "until": "2026-11-22" }`.

### Methoden

| Methode | Zweck |
| --- | --- |
| `Shipment/get`, `Shipment/changes` | Standard (RFC 8620) |
| `Shipment/query`, `Shipment/queryChanges` | Filter `status` (Liste), `active` (alles außer zugestellt/zurück/storniert), `deliveredAfter`, `expectedBefore`, `hidden`, `carrier`. Sortierung `expected`, `updated` |
| `Shipment/set` | **update**: `title`, `hidden`; plus **`addEvents`** (Liste von Events mit `at`, `status`, `text`, `location`). Der Server setzt `source: "client"` und `clientName` aus dem Namen des App-Passworts/OAuth-Clients. **create**: von Hand ein Paket aus Carrier und Nummer anlegen (z. B. App „Sendung hinzufügen“). **destroy**: löschen |
| `Shipment/rescan` | `{ "days": 14 }` startet den Rückblick, antwortet sofort mit dem Job-Status |

Rechte: wie Mail. Ein App-Passwort mit der Nutzung `mail` reicht, eigene Nutzung nicht nötig.
Geteilte Postfächer (Gruppen, `info@`): Pakete gehören zum Konto, in dessen Postfach die Mail lag.
Wer das Postfach lesen darf, sieht sie.

**Live-Status von außen:** `addEvents` ist der Weg, über den Home Assistant (oder später die App)
den Status von den Carrier-APIs zurückschreibt. Dieselben Regeln wie in 2: Der Status geht nur
vorwärts, Duplikate (gleiches `at` + `status`) werden ignoriert, höchstens 200 Events pro Paket.

---

## 5. Eingebaute Feiertags- und Kalenderwochen-Kalender

Wunsch: Feiertage und Kalenderwochen ohne fremden Dienst (früher kamen sie von Google). Vorbild ist
der Geburtstagskalender (`docs/birthdays.md`, `uwumail-store/src/birthdays.rs`): vom Server
erzeugt, nur lesbar, über CalDAV und JMAP sichtbar.

- **Feiertage**: lokal berechnet, keine Netzabfrage. Bundesweite Feiertage plus Länder-Feiertage
  für alle 16 Bundesländer (Heilige Drei Könige, Frauentag Berlin/MV, Fronleichnam, Mariä
  Himmelfahrt, Reformationstag, Allerheiligen, Buß- und Bettag Sachsen, Weltkindertag Thüringen).
  Ostern per Gauß/Computus. Optional Österreich und Schweiz (Kantone grob) als Erweiterung.
  Zeitraum: Vorjahr bis +3 Jahre, wird beim Jahreswechsel fortgeschrieben.
- **Kalenderwochen**: ganztägige Wochen-Events „KW 46“ von Montag bis Sonntag (ISO-8601).
- **Einstellung pro Person** im Portal (Mein Konto → Kalender) und über JMAP-Settings:
  `holidays: { country: "DE", region: "DE-NW" } | null` und `weekNumbers: true|false`.
  Standard: aus. Beim Einschalten den Kalender anlegen, beim Ausschalten löschen.
- Gleiches Verhalten wie abonnierte Kalender: zählt nicht für Frei/Belegt, wird nie
  Standardkalender, keine Erinnerungen, Name und Farbe änderbar.
- **Katalog für Abos** (zusätzlich): Unter „Kalender abonnieren“ eine kleine Liste kostenloser
  iCal-Quellen anbieten (z. B. Schulferien je Bundesland). Vor dem Aufnehmen Lizenz und
  Erreichbarkeit jeder Quelle prüfen und im Code notieren. Die Liste kommt als statische Datei mit
  dem Release, nicht von einem eigenen Dienst.

---

## 6. Portal, Webmail, App

Reihenfolge: Server-Release zuerst, dann Webmail (Pin anheben), dann App.

- **Portal:** Einstellungen aus 3 und 5. Die Funktion „Pakete“ erscheint in der Feature-Liste in
  `README.md`/`docs/features.md`.
- **Webmail** (`UwUMail-Webmail`): Ansicht **„Pakete“** in der Navigation mit Karten pro Paket
  (Carrier-Logo, Shop, Status als Fortschritt
  `bestellt → versendet → unterwegs → in Zustellung → zugestellt`, Liefertag/Fenster,
  Abholort mit Frist, Button „Sendung verfolgen“, Verlauf aufklappbar, Link zur Mail).
  Bereiche „Heute“, „Unterwegs“, „Zugestellt (letzte 7 Tage)“. In der Mail-Ansicht einer
  Versandmail ein Hinweis-Chip „Paket: in Zustellung“. Demo-Backend (`src/backend/demo-*.ts`)
  mitpflegen.
- **App** (`UwUMail-Client`): gleiche Ansicht, außerdem lokale Benachrichtigung bei
  `outForDelivery`/`delivered`/`readyForPickup` (abschaltbar). Für Nicht-UwUMail-Konten in der App
  läuft `shipment()` aus dem kopierten Labels-Crate lokal, nach dem Ändern also neu kopieren.

---

## 7. Release

Nach `CLAUDE.md`: Version `0.36.0` in `Cargo.toml`, Abschnitt im `CHANGELOG.md`, neue Doku
`docs/jmap-shipments.md` und `docs/holidays.md`, `docs/labels.md` und `docs/features.md`
aktualisieren, Security-Review über den Diff und `docs/security-audit-0.36.0.md`. Schwerpunkte:

- Parser auf Mailinhalt (Regex-Laufzeit, Längenlimits, keine Panics bei kaputtem HTML)
- `Shipment/set addEvents` als Schreibweg von außen (Limits, Rechte, Größenbegrenzung von `text`)
- Tracking-Links nie serverseitig aufrufen

Abschluss-Kriterien:

- [ ] Alle Tests grün (`cargo test`, Clippy `-D warnings`, `--locked`), Smoke-Test mit `dev/seed.sh` + Beispiel-Versandmails
- [ ] Pro Carrier aus Teil 1 mindestens eine Test-Mail je Status, Shopify und Amazon dabei
- [ ] Push: Neue Versandmail führt in einer offenen EventSource zu einer `Shipment`-State-Änderung
- [ ] Rückblick über 14 Tage legt die erwarteten Pakete an, ein zweiter Lauf ändert nichts
- [ ] Feiertage NRW, BY und SN für 2026 und 2027 stimmen mit amtlicher Liste überein, KW 53 (2026) richtig
- [ ] Labels-Crate in `UwUMail-Client` neu kopiert (eigener PR dort)

---

## Offene Punkte

- **Carrier-Logos:** eigene SVGs oder neutrale Icons? (Markenrecht: lieber neutrale Icons mit
  Carrier-Name.)
- **Benachrichtigungen vom Server** (Web Push „Paket kommt heute“) zusätzlich zur App? Erst mal
  nein. Home Assistant übernimmt Ansagen und Push.
- **Mehrere Empfänger in einem Haushalt:** Pakete aus geteilten Postfächern doppelt anzeigen oder
  zusammenlegen? Vorerst getrennt pro Konto.

## Gegenstück: Home Assistant

Die Integration (eigenes privates Repo) baut Claude in der HA-Session. Sie nutzt:

- Session `/.well-known/jmap` mit App-Passwort (`mail`)
- `Shipment/query` + `Shipment/get` (aktive und letzte 2 Tage zugestellte Pakete)
- EventSource `types=Shipment` für Sofort-Updates
- `Shipment/set addEvents` für den Live-Status aus DHL- und UPS-API sowie den öffentlichen
  Sendungsverfolgungen von DPD, GLS und Hermes

Bis der Server-Release da ist, wird sie gegen ein Mock des Objekts aus 4 entwickelt. **Das Objekt
und die Methodennamen bitte nur in Absprache ändern.**
