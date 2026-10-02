# JMAP: signatures per domain (`urn:uwumail:jmap:signatures`)

UwUMail's own extension for the signature settings described in [signatures.md](signatures.md).
Mail programs that only insert signatures need none of it: `Identity/get` hands out each address's
effective signature with the placeholders filled.

## Capability

In the session and the account's capabilities:

```json
"urn:uwumail:jmap:signatures": {
  "maxSize": 262144,
  "maxChanges": 500,
  "placeholders": ["name", "adresse", "address", "email", "domain"],
  "allDomains": "*"
}
```

## SignatureSettings/get

Arguments: `accountId`. Answer:

```json
{
  "accountId": "a3",
  "state": "812",
  "allDomains": { "text": "Mini", "html": "" },
  "domains": [
    { "domain": "example.org", "addressCount": 2,
      "signature": { "text": "{name}\n{adresse}", "html": "<p>{name}</p>" },
      "company": { "mode": "footer", "text": "Beispiel GmbH", "html": "" },
      "source": "domain" }
  ],
  "identities": [
    { "id": "i4", "name": "Mini", "email": "info@example.org", "domain": "example.org",
      "signature": null,
      "effective": { "text": "Mini\ninfo@example.org", "html": "<p>Mini</p>" },
      "source": "domain" }
  ]
}
```

- `domains`: the domains of the account's sending addresses. `signature` is the person's own for
  the domain (placeholders as written), `company` the admin's company signature when it is not
  off, `source` what addresses without their own use: `domain`, `allDomains`, `company` or `none`.
- `identities`: `signature` is the address's own (null: it follows its domain), `effective` what
  it sends with (placeholders filled), `source` additionally `identity`.

## SignatureSettings/set

Arguments: `accountId`, optional `ifInState`, `domains` and `identities`, each an object whose
values are `{ "text", "html" }` (both optional strings) or `null` to remove:

```json
["SignatureSettings/set", { "accountId": "a3",
  "domains": { "*": { "text": "Mini", "html": "" }, "example.org": null },
  "identities": { "i4": null } }, "0"]
```

`"*"` is the signature for every domain. Removing an identity's signature makes it follow its
domain again. All or nothing: an unknown domain (not one of the account's addresses), someone
else's identity, a value that is no signature or one over `maxSize` bytes answer
`invalidArguments` and change nothing. The answer is `{ accountId, oldState, newState }`; every
affected `Identity` changes state, so clients refetch it.
