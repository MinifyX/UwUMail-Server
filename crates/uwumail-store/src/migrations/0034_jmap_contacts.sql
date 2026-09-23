-- JMAP Contacts on the CardDAV address books (docs/jmap-contacts.md).
--
-- The address book new cards go into when a client has no better idea: at most one per account,
-- like the default calendar.
UPDATE dav_collections SET is_default = 1
WHERE kind = 'addressbook'
  AND id = (SELECT first.id FROM dav_collections first
            WHERE first.account_id = dav_collections.account_id AND first.kind = 'addressbook'
            ORDER BY first.sort_order, first.id LIMIT 1);
