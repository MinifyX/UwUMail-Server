#!/usr/bin/env python3
"""Writes the synthetic spam filter corpus (see README.md).

Deterministic: a fixed seed and fixed dates, so running it twice gives the same files. Stdlib only.
Every name, address and text is invented; only reserved domains (RFC 2606/6761) and documentation
addresses are used. `--check` verifies that for the written files.
"""

import base64
import email
import email.policy
import os
import random
import re
import sys
from email.header import Header
from email.utils import format_datetime
from datetime import datetime, timedelta, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
NOW = datetime(2026, 9, 16, 10, 0, 0, tzinfo=timezone.utc)
READER = "Max Muster <max@uwu.example>"
rng = random.Random(2210)

FIRST = ["Anna", "Jonas", "Lea", "Felix", "Mia", "Paul", "Emma", "Lukas", "Sophie", "Ben", "Clara", "Tom", "Ida", "Noah", "Greta"]
LAST = ["Becker", "Wagner", "Hoffmann", "Schäfer", "Koch", "Richter", "Klein", "Wolf", "Neumann", "Krüger", "Lange", "Brandt"]
EN_FIRST = ["Olivia", "James", "Amelia", "Oliver", "Harper", "Henry", "Ella", "Jack", "Grace", "Leo"]
EN_LAST = ["Carter", "Hughes", "Bennett", "Foster", "Turner", "Parker", "Reed", "Morgan"]


def person(lang="de"):
    if lang == "de":
        return rng.choice(FIRST), rng.choice(LAST)
    return rng.choice(EN_FIRST), rng.choice(EN_LAST)


def amount():
    return f"{rng.randint(5, 480)},{rng.randint(0, 99):02d}"


def number(digits=8):
    return "".join(str(rng.randint(0, 9)) for _ in range(digits))


def header(value):
    try:
        value.encode("ascii")
        return value
    except UnicodeEncodeError:
        return Header(value, "utf-8").encode()


def address(name, addr):
    if not name:
        return addr
    if any(c in name for c in ',;:<>@"'):
        return f'"{name}" <{addr}>'
    return f"{header(name)} <{addr}>"


def qp_or_8bit(text):
    return text.replace("\n", "\r\n")


def b64(text):
    data = base64.b64encode(text.encode("utf-8")).decode("ascii")
    return "\r\n".join(data[i:i + 76] for i in range(0, len(data), 76))


class Mail:
    def __init__(self, cls, lang, note, auth="spf=pass dkim=pass dmarc=pass", sender="unknown", contacts=None):
        self.cls, self.lang, self.note, self.auth, self.sender, self.contacts = cls, lang, note, auth, sender, contacts
        self.headers = []
        self.body = ""

    def add(self, name, value):
        self.headers.append((name, value))
        return self

    def render(self):
        out = [
            f"X-Corpus-Class: {self.cls}",
            f"X-Corpus-Lang: {self.lang}",
            f"X-Corpus-Auth: {self.auth}",
            f"X-Corpus-Sender: {self.sender}",
        ]
        if self.contacts:
            out.append(f"X-Corpus-Contacts: {self.contacts}")
        out.append(f"X-Corpus-Note: {self.note}")
        out += [f"{name}: {value}" for name, value in self.headers]
        return "\r\n".join(out) + "\r\n\r\n" + self.body


def base(m, from_name, from_addr, subject, hours_ago=None, date=True, message_id=True, to=READER):
    when = NOW - timedelta(hours=hours_ago if hours_ago is not None else rng.randint(1, 70), minutes=rng.randint(0, 59))
    m.add("From", address(from_name, from_addr))
    m.add("To", to)
    m.add("Subject", header(subject))
    if date is True:
        m.add("Date", format_datetime(when))
    elif date:
        m.add("Date", date)
    if message_id:
        m.add("Message-ID", f"<{number(12)}.{rng.randint(100, 999)}@{from_addr.split('@')[1]}>")
    m.add("MIME-Version", "1.0")
    return m


def plain(m, text, encoding="8bit"):
    m.add("Content-Type", "text/plain; charset=utf-8")
    if encoding == "base64":
        m.add("Content-Transfer-Encoding", "base64")
        m.body = b64(text) + "\r\n"
    else:
        m.add("Content-Transfer-Encoding", "8bit")
        m.body = qp_or_8bit(text) + "\r\n"
    return m


def html_only(m, html):
    m.add("Content-Type", "text/html; charset=utf-8")
    m.add("Content-Transfer-Encoding", "8bit")
    m.body = qp_or_8bit(html) + "\r\n"
    return m


def alternative(m, text, html):
    boundary = "alt-" + number(10)
    m.add("Content-Type", f'multipart/alternative; boundary="{boundary}"')
    m.body = (
        f"--{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n"
        f"{qp_or_8bit(text)}\r\n--{boundary}\r\nContent-Type: text/html; charset=utf-8\r\n"
        f"Content-Transfer-Encoding: 8bit\r\n\r\n{qp_or_8bit(html)}\r\n--{boundary}--\r\n"
    )
    return m


def with_attachment(m, text, filename, ctype, data, html=None):
    boundary = "mix-" + number(10)
    m.add("Content-Type", f'multipart/mixed; boundary="{boundary}"')
    first = f"Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{qp_or_8bit(text)}"
    if html is not None:
        first = f"Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{qp_or_8bit(html)}"
    payload = base64.b64encode(data).decode("ascii")
    m.body = (
        f"--{boundary}\r\n{first}\r\n--{boundary}\r\nContent-Type: {ctype}; name=\"{filename}\"\r\n"
        f"Content-Disposition: attachment; filename=\"{filename}\"\r\nContent-Transfer-Encoding: base64\r\n\r\n"
        f"{payload}\r\n--{boundary}--\r\n"
    )
    return m


PDF = b"%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF\n"
ICS = (
    "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//corpus//EN\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\n"
    "UID:{uid}@calendar.example\r\nDTSTART:20260918T090000Z\r\nDTEND:20260918T100000Z\r\nSUMMARY:{summary}\r\n"
    "END:VEVENT\r\nEND:VCALENDAR\r\n"
)

AUTH_PASS = "spf=pass dkim=pass dmarc=pass"
AUTH_NONE = "spf=none dkim=none dmarc=none"
AUTH_SPF = "spf=pass dkim=none dmarc=none"
AUTH_FAIL = "spf=fail dkim=none dmarc=fail"
AUTH_SOFT = "spf=softfail dkim=none dmarc=none"

mails = []

# ---------------------------------------------------------------- ham

SHOPS = [
    ("Gartenwelt", "gartenwelt.example", "de"),
    ("Kaffeerösterei Bohne", "bohne.example", "de"),
    ("Buchladen Seite 7", "seite7.example", "de"),
    ("Outdoor Nord", "outdoor-nord.example", "de"),
    ("Tea & Leaf", "teaandleaf.example", "en"),
    ("Pixel Supply", "pixelsupply.example", "en"),
    ("Bike Barn", "bikebarn.example", "en"),
]

for i, (shop, domain, lang) in enumerate(SHOPS):
    for variant in range(2):
        auth = AUTH_PASS if variant == 0 else rng.choice([AUTH_SPF, AUTH_NONE])
        m = Mail("ham", lang, f"newsletter of {shop} with tracking links showing its own address", auth,
                 sender="known" if variant == 0 else "unknown")
        subj = (["Neu im Sortiment: Herbstauswahl", "Nur heute: 20 % auf alles", "Unsere Empfehlungen für dich"]
                if lang == "de" else ["New arrivals for autumn", "Last chance: 20% off everything", "Picked for you"])
        base(m, shop, f"news@{domain}", rng.choice(subj))
        m.add("List-Unsubscribe", f"<https://{domain}/abmelden?u={number(6)}>")
        m.add("List-Unsubscribe-Post", "List-Unsubscribe=One-Click")
        track = f"https://click.mailer.example/t/{number(10)}"
        hidden = "Die besten Angebote der Woche – jetzt entdecken." if lang == "de" else "This week's best offers – take a look."
        if lang == "de":
            text = f"Hallo Max,\n\nunsere neuen Produkte sind da. Alle Angebote: https://{domain}/angebote\n\nViele Grüße\n{shop}\n\nAbmelden: https://{domain}/abmelden"
            html = (f'<div style="display:none">{hidden}</div><p>Hallo Max,</p><p>unsere neuen Produkte sind da.</p>'
                    f'<p><a href="{track}">https://{domain}/angebote</a></p><p><a href="{track}x">Jetzt ansehen</a></p>'
                    f'<p style="font-size:11px"><a href="https://{domain}/abmelden">Abmelden</a></p>')
        else:
            text = f"Hi Max,\n\nour new products are here. All offers: https://{domain}/offers\n\nCheers,\n{shop}\n\nUnsubscribe: https://{domain}/unsubscribe"
            html = (f'<div style="display:none">{hidden}</div><p>Hi Max,</p><p>our new products are here.</p>'
                    f'<p><a href="{track}">www.{domain}/offers</a></p><p><a href="{track}x">Shop now</a></p>'
                    f'<p style="font-size:11px"><a href="https://{domain}/unsubscribe">Unsubscribe</a></p>')
        if variant == 1 and i % 2 == 0:
            html_only(m, html)
        else:
            alternative(m, text, html)
        mails.append(m)

INVOICERS = [
    ("Stadtwerke Musterstadt", "rechnung@stadtwerke.example", "de", "Ihre Rechnung {n}"),
    ("Hosting Nord GmbH", "billing@hosting-nord.example", "de", "Rechnung {n} – Zahlung dankend erhalten"),
    ("Mobilfunk Plus", "service@mobilfunk-plus.example", "de", "Ihre Mobilfunkrechnung September"),
    ("Cloudbox", "billing@cloudbox.example", "en", "Your receipt #{n}"),
    ("Streamly", "billing@streamly.example", "en", "Your subscription has been renewed"),
    ("Versicherung Sonnenschein", "kundenservice@versicherung.example", "de", "Beitragsrechnung {n}"),
]
for name, addr, lang, subj in INVOICERS:
    for variant in range(2):
        n = number(7)
        a = amount()
        m = Mail("ham", lang, "invoice with PDF from a known sender", AUTH_PASS, sender="known")
        base(m, name, addr, subj.format(n=n))
        if lang == "de":
            text = (f"Guten Tag Max Muster,\n\nanbei erhalten Sie Ihre Rechnung Nr. {n} über {a} EUR.\n"
                    + ("Der Betrag wird am 25.09.2026 von Ihrem Konto abgebucht.\n" if variant == 0 else
                       "Ihre Zahlung haben wir dankend erhalten. Es ist nichts weiter zu tun.\n")
                    + f"\nIhre Rechnungen finden Sie auch im Kundenportal: https://{addr.split('@')[1]}/portal\n\nMit freundlichen Grüßen\n{name}")
        else:
            text = (f"Hello Max,\n\nthank you for your payment of {a.replace(',', '.')} EUR. Your receipt #{n} is attached.\n"
                    f"You can manage your plan at https://{addr.split('@')[1]}/account\n\nBest regards,\n{name}")
        with_attachment(m, text, f"Rechnung_{n}.pdf" if lang == "de" else f"receipt_{n}.pdf", "application/pdf", PDF)
        mails.append(m)

# a polite reminder (Mahnung) from a utility
m = Mail("ham", "de", "legitimate payment reminder from a known utility", AUTH_PASS, sender="known")
base(m, "Stadtwerke Musterstadt", "rechnung@stadtwerke.example", "Zahlungserinnerung zur Rechnung 4402213")
plain(m, "Guten Tag Max Muster,\n\nleider konnten wir für die Rechnung 4402213 über 87,20 EUR noch keinen Zahlungseingang feststellen. "
      "Bitte überweisen Sie den Betrag bis zum 30.09.2026. Sollten Sie bereits gezahlt haben, betrachten Sie dieses Schreiben als gegenstandslos.\n\n"
      "Ihr Kundenportal: https://stadtwerke.example/portal\n\nFreundliche Grüße\nIhre Stadtwerke")
mails.append(m)

CARRIERS = [("Paketdienst Flink", "info@paket.example", "de"), ("Versand24", "status@versand.example", "de"),
            ("ParcelGo", "tracking@parcelgo.example", "en")]
for name, addr, lang in CARRIERS:
    for variant in range(3):
        code = "JJD" + number(14)
        m = Mail("ham", lang, "shipping notice with tracking link to the carrier's own site",
                 AUTH_PASS if variant != 2 else AUTH_SPF, sender="known" if variant == 0 else "unknown")
        dom = addr.split("@")[1]
        if lang == "de":
            base(m, name, addr, rng.choice(["Ihr Paket ist unterwegs", "Ihre Sendung kommt heute", "Zustellung verschoben"]))
            text = f"Hallo Max Muster,\n\nIhre Sendung {code} ist unterwegs und kommt voraussichtlich morgen zwischen 10 und 14 Uhr.\nSendungsverfolgung: https://{dom}/verfolgen/{code}\n\nIhr {name}"
            html = f"<p>Hallo Max Muster,</p><p>Ihre Sendung <b>{code}</b> ist unterwegs.</p><p><a href=\"https://{dom}/verfolgen/{code}\">Sendung verfolgen</a></p>"
        else:
            base(m, name, addr, rng.choice(["Your parcel is on its way", "Out for delivery today", "Delivery rescheduled"]))
            text = f"Hi Max,\n\nyour parcel {code} is on its way and should arrive tomorrow.\nTrack it: https://{dom}/track/{code}\n\n{name}"
            html = f"<p>Hi Max,</p><p>your parcel <b>{code}</b> is on its way.</p><p><a href=\"https://{dom}/track/{code}\">Track your parcel</a></p>"
        alternative(m, text, html)
        mails.append(m)

SERVICES = [("Codeforge", "noreply@codeforge.example", "en"), ("Vereinsportal", "noreply@vereinsportal.example", "de"),
            ("Fotobox", "account@fotobox.example", "de"), ("Notely", "security@notely.example", "en")]
for name, addr, lang in SERVICES:
    dom = addr.split("@")[1]
    kinds = ["reset", "login", "verify", "code"]
    for kind in kinds:
        m = Mail("ham", lang, f"legitimate account mail ({kind}) with links to the service's own domain", AUTH_PASS, sender="known")
        if lang == "de":
            subjects = {"reset": "Passwort zurücksetzen", "login": "Neue Anmeldung bei deinem Konto",
                        "verify": "Bitte bestätige deine E-Mail-Adresse", "code": f"Dein Bestätigungscode: {number(6)}"}
            bodies = {
                "reset": f"Hallo Max,\n\ndu hast angefordert, dein Passwort zurückzusetzen. Das geht hier: https://{dom}/reset/{number(16)}\nDer Link gilt 30 Minuten. Warst du das nicht, ignoriere diese Mail.",
                "login": f"Hallo Max,\n\nwir haben eine neue Anmeldung bei deinem Konto bemerkt (Firefox, Berlin). Warst du das nicht, ändere dein Passwort unter https://{dom}/sicherheit",
                "verify": f"Hallo Max,\n\ndanke für deine Registrierung. Bitte bestätige deine E-Mail-Adresse: https://{dom}/bestaetigen/{number(20)}",
                "code": f"Hallo Max,\n\ndein Code lautet {number(6)}. Er gilt 10 Minuten. Gib ihn niemals weiter.",
            }
        else:
            subjects = {"reset": "Reset your password", "login": "Unusual sign-in to your account",
                        "verify": "Verify your email address", "code": f"Your sign-in code is {number(6)}"}
            bodies = {
                "reset": f"Hi Max,\n\nsomeone asked to reset your password. Reset it here: https://{dom}/reset/{number(16)}\nIf this wasn't you, you can ignore this email.",
                "login": f"Hi Max,\n\nwe noticed an unusual sign-in to your account from a new device. If this was you, nothing to do. Otherwise secure your account at https://{dom}/security",
                "verify": f"Hi Max,\n\nthanks for signing up. Please verify your email address: https://{dom}/verify/{number(20)}",
                "code": f"Hi Max,\n\nyour code is {number(6)}. It expires in 10 minutes. Never share it.",
            }
        base(m, name, addr, subjects[kind])
        plain(m, bodies[kind])
        mails.append(m)

FRIENDS_DE = [
    ("Hey Max, wie sieht es Samstag aus? Wir grillen ab 18 Uhr bei uns im Garten. Bring gern was zu trinken mit.\n\nLG", "Samstag?"),
    ("Hallo Max,\n\nanbei der Link zu den Fotos vom Urlaub: https://fotos.example/album/{n}\nSind ganz schön geworden!\n\nLiebe Grüße", "Fotos"),
    ("Moin,\n\nkannst du mir das Buch nächste Woche zurückgeben? Ich brauche es für die Prüfung.\n\nDanke dir!", "Buch"),
    ("Hallo zusammen,\n\ndas Elterntreffen ist am Donnerstag um 19:30 Uhr in der Aula. Bitte gebt kurz Bescheid, ob ihr kommt.\n\nViele Grüße", "Elterntreffen Donnerstag"),
    ("Hi Max,\n\nok, passt für mich. Bis morgen!\n\n", "WG: OK"),
    ("Lieber Max,\n\nalles Gute zum Geburtstag! Ich hoffe, du hast einen schönen Tag. Wir müssen bald mal wieder telefonieren.\n\nDeine Tante", "Herzlichen Glückwunsch"),
]
for idx, (text, subj) in enumerate(FRIENDS_DE):
    for variant in range(2):
        f, l = person("de")
        m = Mail("ham", "de", "personal mail from a friend or relative", rng.choice([AUTH_SPF, AUTH_NONE, AUTH_PASS]),
                 sender=rng.choice(["contact", "known"]))
        base(m, f"{f} {l}", f"{f.lower()}.{l.lower().replace('ä', 'ae').replace('ü', 'ue')}@mail.example", subj)
        body = text.format(n=number(6)) + f"\n{f}"
        if variant == 1:
            body += "\n\nAm 14.09.2026 um 18:02 schrieb Max Muster:\n> Hallo, wie geht's?\n> Max"
        plain(m, body)
        mails.append(m)

FRIENDS_EN = [
    ("Hi Max,\n\nare we still on for lunch on Friday? I booked a table for 12:30.\n\nCheers", "Lunch Friday"),
    ("Hey,\n\nhere are the slides from today's talk: https://share.example/s/{n}\n\nLet me know what you think.", "Slides"),
    ("Max,\n\nthanks again for helping with the move. Pizza is on me next time!\n\nBest", "Thank you!"),
    ("Hi all,\n\nreminder: the book club meets on Tuesday at 7pm. We're discussing chapters 4 to 6.\n\nSee you", "Book club Tuesday"),
]
for text, subj in FRIENDS_EN:
    for variant in range(2):
        f, l = person("en")
        m = Mail("ham", "en", "personal mail in English", rng.choice([AUTH_SPF, AUTH_PASS, AUTH_NONE]), sender="contact")
        base(m, f"{f} {l}", f"{f.lower()}@{rng.choice(['mail.example', 'post.example', 'home.test'])}", subj)
        body = text.format(n=number(6)) + f"\n{f}"
        if variant == 1:
            body += "\n\nOn Mon, 14 Sep 2026 at 09:12, Max Muster wrote:\n> Sounds good!"
        plain(m, body)
        mails.append(m)

# business mails
for i in range(3):
    f, l = person("de")
    m = Mail("ham", "de", "meeting invitation with a calendar part", AUTH_PASS, sender="known")
    base(m, f"{f} {l}", f"{f.lower()}@firma.example", "Einladung: Projektbesprechung Q4")
    boundary = "alt-" + number(10)
    m.add("Content-Type", f'multipart/alternative; boundary="{boundary}"')
    ics = ICS.format(uid=number(10), summary="Projektbesprechung Q4")
    m.body = (f"--{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHallo Max,\r\nich lade dich zur Projektbesprechung am Freitag um 11 Uhr ein.\r\n\r\n{f}\r\n"
              f"--{boundary}\r\nContent-Type: text/calendar; charset=utf-8; method=REQUEST\r\n\r\n{ics}--{boundary}--\r\n")
    mails.append(m)

for i in range(2):
    f, l = person("de")
    m = Mail("ham", "de", "job application with PDF attachment from an unknown person", AUTH_SPF, sender="unknown")
    base(m, f"{f} {l}", f"{f.lower()}.{l.lower().replace('ä', 'ae').replace('ü', 'ue').replace('ö', 'oe')}@post.example", "Bewerbung als Werkstudent")
    with_attachment(m, f"Sehr geehrte Damen und Herren,\n\nhiermit bewerbe ich mich um die ausgeschriebene Stelle. Meine Unterlagen finden Sie im Anhang.\n\nMit freundlichen Grüßen\n{f} {l}",
                    "Bewerbung.pdf", "application/pdf", PDF)
    mails.append(m)

for lang in ["de", "en"]:
    for i in range(2):
        m = Mail("ham", lang, "support ticket reply, Reply-To at the helpdesk of the same company", AUTH_PASS, sender="known")
        base(m, "Kundensupport" if lang == "de" else "Customer Support", "support@softwarehaus.example",
             f"[Ticket #{number(6)}] " + ("Ihre Anfrage" if lang == "de" else "Your request"))
        m.add("Reply-To", "ticket@helpdesk.softwarehaus.example")
        plain(m, "Hallo Max,\n\nwir haben das Problem gefunden und behoben. Bitte prüfe, ob alles wieder funktioniert.\n\nDein Support-Team"
              if lang == "de" else "Hi Max,\n\nwe found the problem and fixed it. Please check that everything works again.\n\nYour support team")
        mails.append(m)

for i in range(4):
    f, l = person("en")
    m = Mail("ham", "en", "mailing list message, From rewritten by the list", AUTH_PASS, sender="known")
    base(m, f"{f} {l} via dev-list", "dev-list@lists.example", rng.choice(["Re: Release planning", "[dev] Build broken on main", "Proposal: new logo", "Re: Meeting notes"]))
    m.add("List-Id", "<dev-list.lists.example>")
    m.add("List-Post", "<mailto:dev-list@lists.example>")
    m.add("Reply-To", "dev-list@lists.example")
    plain(m, f"I think we should wait until the tests pass again. See https://code.example/project/issues/{number(3)}\n\n-- \n{f}\n\n_______________________________________________\ndev-list mailing list\nhttps://lists.example/listinfo/dev-list")
    mails.append(m)

for i in range(2):
    m = Mail("ham", "de", "forwarded mail from a colleague", AUTH_PASS, sender="contact")
    f, l = person("de")
    base(m, f"{f} {l}", f"{f.lower()}@firma.example", "WG: Angebot Druckerei")
    plain(m, f"Hi Max,\n\nzur Info, siehe unten.\n\n{f}\n\n-------- Weitergeleitete Nachricht --------\nBetreff: Angebot Druckerei\nVon: Druckerei Punkt <angebot@druckerei.example>\n\nSehr geehrte Damen und Herren,\nanbei unser Angebot über 500 Flyer.")
    mails.append(m)

NOTIFY = [
    ("en", "CI", "ci@builds.example", "Build failed: main #{n}", "Build #{n} of project uwu failed.\nSee the log at https://builds.example/uwu/{n}\n"),
    ("en", "Calendar", "reminder@calendar.example", "Reminder: Dentist tomorrow 09:00", "This is a reminder for your event \"Dentist\" tomorrow at 09:00.\n"),
    ("de", "Bank am See", "info@bank.example", "Ihr Kontoauszug ist verfügbar", "Guten Tag Max Muster,\n\nIhr neuer Kontoauszug steht im Online-Banking bereit: https://bank.example/login\n\nIhre Bank am See"),
    ("de", "Praxis Dr. Sommer", "termine@praxis-sommer.example", "Terminerinnerung", "Guten Tag,\n\nwir erinnern Sie an Ihren Termin am 18.09.2026 um 08:30 Uhr. Bitte bringen Sie Ihre Versichertenkarte mit.\n\nIhr Praxisteam"),
    ("de", "Bibliothek", "ausleihe@bibliothek.example", "Leihfrist läuft ab", "Hallo Max Muster,\n\ndie Leihfrist für 2 Medien läuft in 3 Tagen ab. Verlängern: https://bibliothek.example/konto\n"),
    ("en", "Cloudbox", "noreply@cloudbox.example", "Your storage is almost full", "Hi Max,\n\nyou are using 95% of your storage. Manage it at https://cloudbox.example/storage\n"),
]
for lang, name, addr, subj, text in NOTIFY:
    for variant in range(2):
        n = number(4)
        m = Mail("ham", lang, "automated notification", AUTH_PASS, sender="known")
        base(m, name, addr, subj.format(n=n))
        plain(m, text.format(n=n))
        mails.append(m)

for i in range(3):
    m = Mail("ham", "de", "club newsletter, UTF-8 text encoded as base64 (legit)", AUTH_SPF, sender="known")
    base(m, "TSV Grünwiese", "vorstand@tsv-gruenwiese.example", "Vereinsnachrichten September")
    m.add("List-Unsubscribe", "<mailto:abmelden@tsv-gruenwiese.example>")
    plain(m, "Liebe Mitglieder,\n\nam Sonntag findet unser Sommerfest statt. Für Kuchen ist gesorgt, Grillgut bringt bitte jeder selbst mit. "
          "Die Jugendabteilung sucht noch Helfer für den Aufbau.\n\nSportliche Grüße\nEuer Vorstand", encoding="base64")
    mails.append(m)

for lang in ["de", "en"]:
    for i in range(3):
        m = Mail("ham", lang, "authenticated marketing mail the reader wanted, with urgency words", AUTH_PASS, sender="known")
        shop, dom, _ = rng.choice(SHOPS)
        base(m, shop, f"angebote@{dom}", "Nur heute: Gratis-Versand!" if lang == "de" else "Last chance: free shipping ends tonight!")
        m.add("List-Unsubscribe", f"<https://{dom}/u/{number(5)}>")
        alternative(m, "Nur heute versenden wir kostenlos. Jetzt zugreifen!" if lang == "de" else "Free shipping ends tonight. Don't miss it!",
                    f'<h1>{"Nur heute!" if lang == "de" else "Last chance!"}</h1><p><a href="https://{dom}/sale">{"Zum Sale" if lang == "de" else "Shop the sale"}</a></p>')
        mails.append(m)

# ---------------------------------------------------------------- spam

SPAM_TEXTS = [
    ("en", "Cheap meds online", "Get your meds without prescription. Best prices guaranteed. Order now: {link}"),
    ("en", "You are our lucky winner!!!", "Congratulations! Your email address was selected and you won USD 950,000. To claim your prize send your full name, address and phone number."),
    ("en", "Double your bitcoin in 7 days", "Our trading bot makes 300% per month. Invest now and become rich. Limited spots: {link}"),
    ("de", "Kredit ohne Schufa – sofort", "Sie brauchen Geld? Wir vergeben Kredite bis 50.000 EUR ohne Schufa, Auszahlung in 24 Stunden. Jetzt anfragen: {link}"),
    ("de", "Ihre Webseite bei Google auf Platz 1", "Sehr geehrte Damen und Herren, wir bringen Ihre Webseite garantiert auf Platz 1. Antworten Sie für ein kostenloses Angebot."),
    ("en", "Hot singles in your area", "Lonely tonight? Thousands of singles are waiting to meet you. Sign up free: {link}"),
    ("de", "Gewinnbenachrichtigung", "Herzlichen Glückwunsch! Sie haben 25.000 EUR gewonnen. Zur Auszahlung senden Sie uns bitte Ihre Bankverbindung und eine Ausweiskopie."),
    ("en", "Business proposal", "Dear friend, I am a lawyer representing a late client who left 12.5 million USD. I need a foreign partner to transfer the funds. You will receive 40%."),
    ("de", "Erbschaft", "Sehr geehrter Freund, ich kontaktiere Sie wegen einer Erbschaft von 8,5 Millionen Euro. Bitte antworten Sie vertraulich an meine private Adresse."),
    ("en", "Lose 15 kg in 2 weeks", "Doctors hate this one trick! Burn fat while you sleep. Order today and get 2 bottles free: {link}"),
    ("de", "Abnehmen ohne Diät", "Mit unserem Wundermittel verlieren Sie 10 Kilo in 14 Tagen. Nur heute 70 % Rabatt: {link}"),
    ("en", "Replica watches 90% off", "Luxury watches at unbeatable prices. Swiss quality replicas. Visit {link}"),
    ("de", "Ihr Gratis-iPhone wartet", "Sie wurden ausgewählt! Nehmen Sie an unserer Umfrage teil und erhalten Sie ein Gratis-Smartphone: {link}"),
    ("en", "Work from home – earn $500/day", "No experience needed. Earn money from home with just your phone. Start today: {link}"),
    ("de", "Solaranlage zum Nulltarif", "Jetzt Förderung sichern! Solaranlage ohne Anzahlung. Nur noch wenige Plätze frei: {link}"),
    ("en", "Your website needs an upgrade", "Hi, I noticed your website is not mobile friendly. We redesign sites for just $199. Reply YES for details."),
    ("de", "Potenzmittel diskret bestellen", "Rezeptfrei und diskret geliefert. Jetzt bestellen: {link}"),
    ("en", "Crypto presale – 1000x guaranteed", "Join the presale of the next big coin. Guaranteed 1000x returns. Buy before it's too late: {link}"),
    ("de", "Investieren wie die Profis", "Verdienen Sie täglich 800 Euro mit unserer KI-Trading-Software. Jetzt kostenlos testen: {link}"),
    ("en", "Your invoice is attached", "Please see the attached invoice and pay within 3 days."),
]
SPAM_DOMAINS = ["promo-mail.example", "best-deals.test", "mega-offers.example", "luckydraw.test", "rich-fast.example",
                "newsletter-blast.test", "a7x9k2.example", "offers4u.test"]
for idx, (lang, subj, text) in enumerate(SPAM_TEXTS):
    for variant in range(2):
        dom = rng.choice(SPAM_DOMAINS)
        auth = rng.choice([AUTH_FAIL, AUTH_NONE, AUTH_SOFT, AUTH_PASS]) if variant == 0 else rng.choice([AUTH_PASS, AUTH_NONE])
        m = Mail("spam", lang, "unsolicited bulk / scam mail", auth, sender="unknown")
        no_date = variant == 1 and idx % 5 == 0
        no_id = variant == 1 and idx % 4 == 1
        future = variant == 0 and idx % 7 == 3
        subject = subj.upper() if (variant == 1 and idx % 3 == 0) else subj
        base(m, rng.choice(["Info", "Special Offer", "Kundenservice", "Gewinnspiel-Team", "Mr. Edward Brown", "Support"]),
             f"{rng.choice(['info', 'offer', 'news', 'contact', 'promo'])}{rng.randint(1, 99)}@{dom}", subject,
             date=False if no_date else (format_datetime(NOW + timedelta(days=5)) if future else True), message_id=not no_id)
        link = rng.choice([f"https://kurz.example/{number(5)}", f"http://{dom}/go/{number(6)}", f"http://203.0.113.{rng.randint(2, 250)}/offer"])
        body = text.format(link=link)
        if "freund" in body.lower() or "friend" in body.lower() or "privat" in body.lower():
            m.add("Reply-To", f"private.{number(4)}@mail.example")
        if "attached invoice" in body:
            with_attachment(m, body, rng.choice(["invoice.zip", "invoice.pdf.exe", "Rechnung.exe"]), "application/octet-stream", b"MZ\x90\x00corpus")
        elif variant == 1 and idx % 2 == 0:
            plain(m, body, encoding="base64")
        elif idx % 3 == 1:
            hidden = " ".join(rng.choice(["lorem", "ipsum", "dolor", "garden", "weather", "piano", "travel"]) for _ in range(60))
            html_only(m, f'<p>{body.replace(link, f"<a href=\"{link}\">{link}</a>")}</p><div style="display:none">{hidden}</div>')
        elif idx % 3 == 2:
            html_only(m, f'<a href="{link}"><img src="http://{dom}/banner.png" width="600" height="400"></a>')
        else:
            plain(m, body)
        mails.append(m)

# ---------------------------------------------------------------- phishing
# Imitations of brands and of the reader's own provider, as filters see them every day. Domains are
# reserved names; the brand names stand for what is being imitated.

def lookalike(name):
    """A punycode label with a Cyrillic letter in place of a Latin one, under .example."""
    swapped = name.replace("a", "а", 1) if "a" in name else name.replace("o", "о", 1)
    return swapped.encode("idna").decode("ascii") + ".example"


PHISH = [
    ("de", "PayPal Service", "service@konto-hilfe.example", "Ihr Konto wurde eingeschränkt",
     "Ihr Konto wurde gesperrt. Bitte bestätigen Sie Ihre Daten innerhalb von 24 Stunden, sonst wird es dauerhaft geschlossen.", "https://konto-check.example/login", "PayPal"),
    ("de", "Sparkasse", "info@sparkasse-sicherheit.example", "Wichtige Sicherheitsüberprüfung",
     "Aufgrund einer Sicherheitsüberprüfung müssen Sie Ihr pushTAN-Verfahren neu registrieren. Ohne Legitimation wird Ihr Zugang gesperrt.", "https://sparkasse-sicherheit.example/legitimation", None),
    ("de", "DHL Paket", "zustellung@dhl-paket-zustellung.example", "Ihr Paket konnte nicht zugestellt werden",
     "Ihre Sendung wartet. Bitte zahlen Sie die Zollgebühr von 1,99 EUR, damit wir sie zustellen können.", "https://dhl-paket-zustellung.example/zahlung", None),
    ("en", "Microsoft 365", "admin@micros0ft-365.example", "Your password expires today",
     "Your password will expire in 24 hours. Keep your current password by confirming your account below.", "https://micros0ft-365.example/keep", None),
    ("en", "Netflix", "billing@netfllx-billing.example", "Payment declined – update your billing",
     "We couldn't process your last payment. Update your payment details to avoid interruption of your membership.", "https://netfllx-billing.example/update", None),
    ("de", "Amazon", "konto@kundenkonto-service.example", "Ihr Amazon-Konto wurde gesperrt",
     "Wir haben ungewöhnliche Aktivität festgestellt. Bitte bestätigen Sie Ihre Identität, um Ihr Konto zu entsperren.", "https://kundenkonto-service.example/verify", "Amazon"),
    ("en", "Apple ID", "noreply@id-verify.example", "Your Apple ID has been locked",
     "Your account has been locked for security reasons. Verify your identity to restore access.", "https://id-verify.example/appleid", "Apple"),
    ("de", "Telekom Rechnung", "rechnung@telekom-kundencenter.example", "Ihre Rechnung konnte nicht abgebucht werden",
     "Die Abbuchung Ihrer letzten Rechnung ist fehlgeschlagen. Bitte aktualisieren Sie Ihre Zahlungsdaten.", "https://telekom-kundencenter.example/zahlung", None),
    ("de", "Volksbank Online", "service@vb-online-sicherheit.example", "Neue Sicherheitsrichtlinie",
     "Ab sofort ist eine Verifizierung erforderlich. Bitte melden Sie sich an und bestätigen Sie Ihre Daten.", "https://vb-online-sicherheit.example/login", "Volksbank"),
    ("en", "eBay", "member@ebay-resolution.example", "Action required on your account",
     "Your account is on hold due to suspicious activity. Confirm your account to continue selling.", "https://ebay-resolution.example/confirm", None),
    ("de", "ING Kundenservice", "kundenservice@ing-sicherheitscenter.example", "Ihr Konto wird gesperrt",
     "Ihr Konto wird gesperrt, wenn Sie Ihre Daten nicht bestätigen.", "https://ing-sicherheitscenter.example/daten", None),
    ("en", "WhatsApp", "support@whatsapp-verify.example", "Verify your account",
     "Your WhatsApp account will be suspended. Verify your account now.", "https://whatsapp-verify.example/verify", None),
]
for lang, name, addr, subj, text, link, _ in PHISH:
    for variant in range(2):
        auth = AUTH_PASS if variant == 0 else rng.choice([AUTH_FAIL, AUTH_NONE, AUTH_SOFT])
        m = Mail("phishing", lang, f"brand imitation ({name}) with credential request", auth, sender="unknown")
        base(m, name, addr, subj)
        greeting = "Sehr geehrter Kunde," if lang == "de" else "Dear customer,"
        button = "Jetzt bestätigen" if lang == "de" else "Confirm now"
        if variant == 0:
            html_only(m, f"<p>{greeting}</p><p>{text}</p><p><a href=\"{link}\">{button}</a></p>")
        else:
            alternative(m, f"{greeting}\n\n{text}\n\n{button}: {link}", f"<p>{greeting}</p><p>{text}</p><p><a href=\"{link}\">{button}</a></p>")
        mails.append(m)

# punycode lookalike senders and links
for brand, label, lang in [("PayPal", "paypal", "de"), ("Amazon", "amazon", "en"), ("Sparkasse", "sparkasse", "de"), ("Microsoft", "microsoft", "en")]:
    dom = lookalike(label)
    m = Mail("phishing", lang, f"punycode lookalike of {brand} as sender and link", AUTH_PASS, sender="unknown")
    base(m, f"{brand} Security", f"security@{dom}", "Sicherheitswarnung" if lang == "de" else "Security alert")
    html_only(m, f"<p>{'Verdächtige Anmeldung erkannt. Bitte bestätigen Sie Ihre Identität.' if lang == 'de' else 'Suspicious activity detected. Please verify your identity.'}</p>"
                 f"<p><a href=\"https://{dom}/signin\">{'Anmelden' if lang == 'de' else 'Sign in'}</a></p>")
    mails.append(m)

# display name containing an address, link text showing a bank but leading elsewhere
for lang in ["de", "en"]:
    for i in range(3):
        m = Mail("phishing", lang, "display name shows a bank address, link text shows the bank, href goes elsewhere",
                 rng.choice([AUTH_NONE, AUTH_PASS, AUTH_FAIL]), sender="unknown")
        evil = rng.choice(["evil.example", "secure-login.test", "konto-update.example"])
        base(m, "service@bank.example", f"alert{i}@{evil}", "Kontowarnung" if lang == "de" else "Account warning")
        target = rng.choice([f"https://{evil}/bank/login", f"http://198.51.100.{rng.randint(2, 250)}/login"])
        html_only(m, f"<p>{'Bitte melden Sie sich an, um eine verdächtige Zahlung zu prüfen:' if lang == 'de' else 'Please sign in to review a suspicious payment:'}</p>"
                     f"<p><a href=\"{target}\">https://www.bank.example/login</a></p>")
        mails.append(m)

# boss / CEO fraud, no links
for lang, contacts, frm, subj, text in [
    ("de", "firma.example", "chef@flrma.example", "Dringend – kurze Aufgabe", "Hallo Max, bist du gerade da? Ich brauche dringend eine Überweisung noch heute an einen neuen Lieferanten. Bitte antworte nur per Mail, ich bin im Termin."),
    ("de", "firma.example", "geschaeftsfuehrung@firma-gmbh.example", "Vertraulich", "Hallo Max, ich brauche für Kunden 5 Geschenkkarten zu je 100 Euro. Kannst du die heute besorgen und mir die Codes schicken? Bitte vertraulich behandeln."),
    ("en", "acme-corp.example", "ceo@acme-c0rp.example", "Quick favour", "Hi Max, are you available? I need you to process a wire transfer today for an acquisition. Keep this confidential. I'll explain later."),
    ("en", "acme-corp.example", "ceo@acme-corp.example", "Urgent request", "Hi Max, please buy 4 gift cards (Google Play card, 200 each) for a client meeting and send me the codes immediately."),
    ("de", "kanzlei-weber.example", "buero@kanzlei-webber.example", "Neue Bankverbindung", "Sehr geehrter Herr Muster, bitte beachten Sie unsere neue Bankverbindung für die offene Rechnung. Überweisen Sie den Betrag umgehend auf das neue Konto."),
]:
    m = Mail("phishing", lang, "CEO/partner fraud without links", AUTH_PASS if "acme-corp.example" not in frm else AUTH_FAIL,
             sender="unknown", contacts=contacts)
    base(m, "Thomas Brandt" if lang == "de" else "Richard Hale", frm, subj)
    if frm.endswith("@acme-corp.example") or frm.endswith("@firma-gmbh.example"):
        m.add("Reply-To", f"boss.{number(4)}@mail.example")
    plain(m, text)
    mails.append(m)

# imitating the reader's own provider
for lang in ["de", "en"]:
    for i in range(2):
        m = Mail("phishing", lang, "fake mailbox-full mail imitating the reader's own domain", rng.choice([AUTH_PASS, AUTH_NONE]), sender="unknown")
        base(m, "uwu.example Postmaster" if lang == "de" else "uwu.example Admin", f"postmaster@uwu-example-mail.test",
             "Ihr Postfach ist voll" if lang == "de" else "Your mailbox is full")
        html_only(m, f"<p>{'Ihr Postfach ist voll. Neue Nachrichten werden abgewiesen. Melden Sie sich an, um Speicher freizugeben.' if lang == 'de' else 'Your mailbox quota is exceeded. Sign in to restore access.'}</p>"
                     f"<p><a href=\"https://webmail-upgrade.test/uwu.example\">{'Speicher erweitern' if lang == 'de' else 'Upgrade storage'}</a></p>")
        mails.append(m)

# HTML attachment phishing
for lang in ["de", "en"]:
    m = Mail("phishing", lang, "login page as HTML attachment", AUTH_NONE, sender="unknown")
    base(m, "Scan Service", "scanner@docs-share.example", "Neues Dokument für Sie" if lang == "de" else "New document shared with you")
    with_attachment(m, "Öffnen Sie das angehängte Dokument und melden Sie sich an, um es anzusehen." if lang == "de" else "Open the attached document and sign in to view it.",
                    "Dokument.html" if lang == "de" else "document.html", "text/html",
                    b"<html><form action='https://collect.example/p'><input name='password'></form></html>")
    mails.append(m)

# reply-to freemail scams imitating a known brand in the display name
for lang in ["de", "en"]:
    m = Mail("phishing", lang, "brand in display name, Reply-To to freemail, asks for data", AUTH_SPF, sender="unknown")
    base(m, "DHL Kundenservice" if lang == "de" else "FedEx Support", "noreply@shipping-notice.example",
         "Zustellung fehlgeschlagen" if lang == "de" else "Delivery failed")
    m.add("Reply-To", f"zustellung{number(3)}@mail.example")
    plain(m, "Ihre Zustellung ist fehlgeschlagen. Bitte antworten Sie mit Ihrer Adresse und Ihren Zahlungsinformationen." if lang == "de"
          else "Your delivery failed. Reply with your address and payment details to reschedule.")
    mails.append(m)


# ---------------------------------------------------------------- writing and checking

RESERVED = re.compile(
    r"^(?:[a-z0-9-]+\.)*(?:example\.(?:com|net|org)|example|test|invalid|localhost)$"
)


def domains_in(text):
    found = set()
    for match in re.finditer(r"@([A-Za-z0-9.-]+\.[A-Za-z0-9-]+)", text):
        found.add(match.group(1).lower().rstrip("."))
    for match in re.finditer(r"(?:https?://|www\.)([A-Za-z0-9.-]+)", text):
        found.add(match.group(1).lower().rstrip("."))
    return found


def check():
    problems = 0
    count = 0
    for cls in ["ham", "spam", "phishing"]:
        for name in sorted(os.listdir(os.path.join(HERE, cls))):
            path = os.path.join(HERE, cls, name)
            raw = open(path, "rb").read()
            count += 1
            message = email.message_from_bytes(raw, policy=email.policy.default)
            if message["X-Corpus-Class"] != cls:
                print(f"{path}: wrong class"); problems += 1
            text = raw.decode("utf-8", "replace")
            for part in message.walk():
                if part.get_content_maintype() == "text":
                    try:
                        text += "\n" + part.get_content()
                    except Exception:  # broken parts are part of the corpus
                        pass
            for domain in domains_in(text):
                if re.fullmatch(r"[0-9.]+", domain):
                    continue  # an address literal, checked below
                if not RESERVED.match(domain):
                    print(f"{path}: not a reserved domain: {domain}"); problems += 1
            for ip in re.findall(r"\b(\d{1,3}\.\d{1,3}\.\d{1,3})\.\d{1,3}\b", text):
                if ip not in ("192.0.2", "198.51.100", "203.0.113"):
                    print(f"{path}: not a documentation address: {ip}.x"); problems += 1
    print(f"checked {count} files, {problems} problems")
    return problems == 0


def write():
    counts = {}
    for cls in ["ham", "spam", "phishing"]:
        directory = os.path.join(HERE, cls)
        os.makedirs(directory, exist_ok=True)
        for old in os.listdir(directory):
            if old.endswith(".eml"):
                os.remove(os.path.join(directory, old))
    numbers = {}
    for m in mails:
        numbers[m.cls] = numbers.get(m.cls, 0) + 1
        slug = re.sub(r"[^a-z0-9]+", "-", m.note.lower()).strip("-")[:40].strip("-")
        path = os.path.join(HERE, m.cls, f"{numbers[m.cls]:03d}-{slug}.eml")
        with open(path, "w", encoding="utf-8", newline="") as out:
            out.write(m.render())
        counts[(m.cls, m.lang)] = counts.get((m.cls, m.lang), 0) + 1
    for key in sorted(counts):
        print(f"{key[0]:9} {key[1]}: {counts[key]}")
    print(f"total: {len(mails)}")


if __name__ == "__main__":
    if "--check" in sys.argv:
        sys.exit(0 if check() else 1)
    write()
    sys.exit(0 if check() else 1)
