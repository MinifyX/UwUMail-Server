# Spam filter corpus

Synthetic mail for measuring the spam filter's rules: `ham/` (wanted mail), `spam/` (unwanted bulk
and scams) and `phishing/` (credential and payment theft, impersonation), in German and English.
`tests/integration/corpus.rs` scores every mail with `uwumail_smtp::score_offline` and checks how
much wanted mail would be greylisted or put into Junk and how much of the rest is caught.

Everything here is invented. Only reserved names (RFC 2606/6761: `example.com/.net/.org`,
`*.example`, `*.test`, `*.invalid`, `*.localhost`) and documentation addresses (192.0.2.0/24,
198.51.100.0/24, 203.0.113.0/24) are used; brand names appear only as what a phishing mail imitates.

## Format

Each `.eml` starts with corpus headers, which the test strips before scoring:

```
X-Corpus-Class: ham | spam | phishing
X-Corpus-Lang: de | en
X-Corpus-Auth: spf=<pass|fail|softfail|none> dkim=<pass|fail|none> dmarc=<pass|fail|none>
X-Corpus-Sender: contact | known | unknown
X-Corpus-Contacts: domains the reader has contacts at (optional)
X-Corpus-Note: what the mail tests
```

The authentication results stand in for what the SMTP server would have checked. `known` and
`contact` senders count as having a good reputation in the test's "warm" run. "Now" is
Wed, 16 Sep 2026 10:00:00 +0000.

The ham is deliberately hard: newsletters whose tracking links show the shop's own address,
HTML-only mail, password resets and sign-in alerts, invoices with attachments, mailing lists that
rewrite From, support replies with another Reply-To, base64-encoded UTF-8 text.

## Regenerating

```
python3 generate.py          # writes all files, then checks them
python3 generate.py --check  # only checks: parses, reserved names, documentation addresses
```

The script is deterministic (fixed seed and dates). Edit the templates there, not the files.

Real mail is never added here; a local corpus of real mail is measured with the ignored test
`corpus::real_mail` (`UWUMAIL_REAL_CORPUS=<dir>`).
