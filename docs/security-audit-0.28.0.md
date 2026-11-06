# Security audit — outbound abuse (0.28.0)

Scope: `git diff main...feat/0.28-outbound-abuse`: the outbound spam scan and
send suspensions (`uwumail-smtp/src/submission.rs`, `abuse.rs`, `outbound.rs`,
`uwumail-store/src/abuse.rs`), the Microsoft hold (`outbound.rs`, `dsn.rs`,
`microsoft.rs`), ARF/JMRP complaints to `abuse@` (`abuse.rs`, `inbound.rs`),
host names in `login_block_exempt` (`abuse.rs`, `uwumail-store/src/blocks.rs`),
the new admin routes (`uwumail-web/src/routes/sending.rs`, `blocks.rs`,
`microsoft.rs`) and their alerts. The web portal was not reviewed.

## Summary

| Severity | Count |
| --- | --- |
| Critical | 0 |
| High | 2 |
| Medium | 2 |
| Low | 5 |
| Info | 3 |

The report parser has no panic paths on outside input, runs on a blocking
thread behind the 4 MB limit and the report readers' semaphore, and every query
is parameterised. The new routes all use the `Admin` extractor; lifting goes
through the CSRF check and the change log. The Microsoft hold only applies to
replies from Microsoft hosts and turns into a normal bounce after 48 hours.

The two high findings let an outsider keep a person's outside sending
suspended. Both are fixed.

## Findings

| ID | Severity | Location | Cause | Fix | Status |
| --- | --- | --- | --- | --- | --- |
| OA-01 | High | `smtp/src/abuse.rs` `sender_of`, `store/src/abuse.rs` `add_abuse_report` | A complaint counted when it passed DMARC (any domain owner can arrange that) and named a Message-ID we sent or carried our valid DKIM signature. Anyone who got mail from an account could report it; vacation replies hand out fresh signed messages on demand. Three reports suspended the account, again and again. | Count only Message-ID matches; count only when the reporter's domain is aligned with one of the original's recipient domains, or is Microsoft's JMRP; at most one counted complaint per reporter domain, account and window; the DKIM fallback names the account but never counts. | fixed: `sent_message_ids.recipient_domains`, `may_complain`, `TRUSTED_FEEDBACK_LOOPS`, `once_per_reporter_since`. Tests `only_the_recipients_side_may_complain`, `one_reporter_domain_counts_once_per_window`, `a_complaint_about_sent_mail_is_kept_and_counts_against_its_sender`. |
| OA-02 | High/Medium | `smtp/src/outbound.rs` `deliver_group` | Every permanent failure of any queue entry with an account counted as a bounce strike: forwarded copies and vacation replies too (an outsider decides those), every 5xx kind, and one strike per recipient. | Count only mail with the account's own return path, only 5.1.0/5.1.1/5.1.10, once per message and recipient domain. | fixed: `is_unknown_user`, return-path owner check, one strike per delivery group. Test `only_unknown_addresses_count_as_bounce_strikes`. |
| OA-03 | Medium | `smtp/src/submission.rs` `submit_checked`, `deliver_locally` | Mail for someone here counted as "inside", so a suspended or spamming account could write to its own forwarding mailbox (or a forwarding address) and the forward went out unscanned. | Before forwarding submitted mail elsewhere, check the sender's suspension and run the outbound scan; otherwise keep the copy here and drop only the outside targets. | fixed: `Smtp::may_leave` (asked once per message). Test `spam_for_other_servers_is_refused_and_strikes_suspend_sending`. |
| OA-04 | Medium/Low | `store/src/abuse.rs` `record_sent_message_id` | The client chooses the Message-ID, and the first account to use one owned it. An account could reuse another's ID so complaints about its spam landed on the other. | A Message-ID two accounts used belongs to neither. | fixed: `account_id` NULL on conflict. Test `a_message_id_two_accounts_used_belongs_to_neither`. |
| OA-05 | Low | `smtp/src/abuse.rs` `capped` | Only unspecified addresses were dropped, so a host name could exempt loopback, private, CGNAT or proxy addresses. | Keep only public addresses; document that whoever controls the name controls the exemption. | fixed: `is_public`. Test `host_addresses_are_distinct_and_capped`. Addresses of a name that stops resolving are kept (by design, so a DNS hiccup does not lock the admin out). |
| OA-06 | Low | `store/src/abuse.rs` `record_send_strike` | Rate-limit strikes counted once per 5 minutes, so a client retrying a refused message suspended a person near the limit in about 15 minutes. | Count rate-limit strikes once per window. | fixed: the limit alone never suspends. Accepted: mail inside the server uses the budget too, and a message with more recipients than `per_hour` is refused with 451 each time. |
| OA-07 | Low | `smtp/src/submission.rs` outbound scan | Forwarding a phishing mail to another provider's abuse desk can score as spam and strike. | Score without attached originals and quotes. | accepted: the refusal names the reason, one such mail is one strike, and admins can lift a suspension. |
| OA-08 | Low | `web/src/routes/microsoft.rs` `issues_json` | The held count loads the whole queue on every admin home page visit. | One `COUNT` query over pending recipients. | accepted for now: the queue is small in normal operation. |
| OA-09 | Low | `smtp/src/inbound.rs` | The message was copied before the 4 MB report size check. | Check the size first. | fixed: `receive_soon` takes a slice and copies after the check. |
| OA-10 | Info | `smtp/src/abuse.rs` `receive_soon` | Complaints share the report readers' permits and the 5000-row cap, so a flood to `abuse@` can crowd out DMARC/TLS reports. | Separate permit. | accepted. |
| OA-11 | Info | `smtp/src/outbound.rs` delay notice | When held attempts alternate with 4xx deferrals, the delay notice can be sent again. | — | accepted: bounded by the 48-hour hold. |
| OA-12 | Info | queue | Mail queued before a suspension still goes out. | Pause the account's queued outside mail. | accepted. |

Checked and found fine: report parsing (no unwrap or slicing on outside input,
`replace_range(..1, "4")` in `dsn.rs` only on ASCII codes), replayed reports
(our DKIM signs Message-ID and From twice), table growth (Message-IDs 30 days
and 255 characters, complaints 180 days or 5000 rows, strikes at most 7 days),
the alert and DSN texts (no header injection), and the exempt host name syntax
check.
