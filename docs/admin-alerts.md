# The server overview: calm view, alerts and statistics

The admin's start page (*Server → Overview*) opens with the health check of the
server (DNS, certificate, sending, storage, login, and the gateway and virus
scanner where there are any; [architecture.md](architecture.md) says what each
area looks at). Around it are three things that help an admin who does not want
to look every day: a calmer view, alerts that come by mail, and statistics.

## Simple or everything

At the top of the overview each admin chooses how much they want to see:

- **Simple** shows one traffic light, the worst of all health areas and open
  alerts, with a sentence saying what it means. Below it are only the things
  that are yellow or red, each with what to do about it and a link to the page
  where it can be done. Then shortcuts to *People*, *Add a person*, *Domains*
  and *Backups*. In the menu, *Server* keeps *Overview* and *Accounts &
  domains*; everything else (queue, spam filter, settings, logs) folds away
  under *More*, one click away and unfolding by itself on those pages.
- **Everything** is the full overview with every area, the tiles, the alerts
  and the numbers, as before.

The choice belongs to the admin, not the server: it is a portal preference
(`adminView`, `simple` or `full`) like language and theme. Admins who never
chose see everything.

## Alerts

Every five minutes (the first time two minutes after the start) the server looks
at itself the way the health overview does, and adds what the overview does not
show:

| Kind | When |
| --- | --- |
| Health areas | A finding of the overview turns yellow or red: DNS records missing or wrong, TLS failures reported by other servers, our own mail failing DMARC, a certificate that expires soon or does not match, mail stuck in the queue, many bounces, the relay or port 25 unreachable, the gateway away, the virus scanner away or out of date, disk space running low, mailboxes nearly full, admins without a second factor |
| Microsoft | Microsoft refuses mail from a sending address or domain (red) or throttles it (yellow); see [microsoft.md](microsoft.md) |
| Backup | The last backup failed (red), or the last successful one is more than two days old (yellow) |
| Certificate | Renewing the Let's Encrypt certificate has been failing for more than a day (yellow); single failures are common and heal on their own |
| Update | A new version is out (information only, never mailed) |

Each of these is an alert, kept per kind and what it is about (a domain's DNS
records are one alert per domain). The list on the overview shows the open ones,
worst first, and folds away the ones that were fine again in the last 90 days.

### Mails to the admins

An admin hears about an alert by mail, in their own language and tone like every
other mail the server writes to its people:

- when it is new,
- when it gets worse (yellow turns red),
- once a day while it stays red, until an admin clicks *Got it*,
- and once more when it is fine again.

An alert only counts as fine when it has not been seen for 15 minutes, so a
value wobbling around a limit does not write a mail each time. Everything one
look finds goes into one mail. The mail comes from `postmaster@` the admin's
domain and lands in their inbox directly, so it arrives even when sending to
other servers is what is broken.

*Got it* stops the daily reminders for that alert (for every admin; the list
says who clicked it). Getting worse or being fine again is still mailed.

Below the alerts each admin chooses which mails they want (`adminAlerts`):
*All* (yellow and red), *Only problems* (red, and the "fine again" of something
that was red), or *None*. The list on the overview is the same for everyone.

## Statistics

*Server → Statistics* shows what happened on the server over the last 30 days
or 12 months:

- **Mail received** from other servers and fetched mailboxes, and how much of it
  went into Junk,
- **Mail turned away** at the door, by reason: unknown recipient, spam, virus,
  rules (DMARC, blocked senders, relaying); and how often a sender was asked to
  come back later (greylisting), which is not counted as turned away since most
  of it comes back,
- **Mail sent** by the server's people, from any app,
- **Handed to other servers**, with how often delivery was tried again later and
  how many recipients were given up on,
- **Failed logins** per protocol (SMTP, IMAP, JMAP, CalDAV/CardDAV, ManageSieve,
  the portal),
- **Storage used** by all mailboxes.

Each chart is one series as bars, with its total for the period, the highest
value labelled, and every bar's number on hover or with the arrow keys once the
chart has focus. *All numbers as a table* has every value of every period.

The server counts in memory as things happen and adds the numbers to the day's
row in the database every minute, and once more when it stops. Days are UTC
days. Only numbers are kept (table `stats_daily`: day, name, value), never who
or what, for 400 days. Counting starts with version 0.14, so earlier days are
empty.

The same counters, and more, are available to Prometheus: see
[metrics.md](metrics.md).
