# JMAP Contacts

UwUMail Server serves the address books people already have over CardDAV as
JMAP Contacts ([RFC 9610](https://www.rfc-editor.org/rfc/rfc9610)), with cards
in JSContact ([RFC 9553](https://www.rfc-editor.org/rfc/rfc9553)). The webmail
and the UwUMail apps use it; phones and Thunderbird keep using CardDAV, and
both see the same address books and cards.

This page says what is supported and how the server behaves where the RFC
leaves room.

## Capability

`urn:ietf:params:jmap:contacts` is in the session's `capabilities` (an empty
object), in `primaryAccounts` and in the account's `accountCapabilities`:

```json
"urn:ietf:params:jmap:contacts": {
  "maxAddressBooksPerCard": 1,
  "mayCreateAddressBook": true
}
```

Only accounts that may use CardDAV get it: when an admin switches CardDAV off
for an account (services have it off from the start), the capability is gone
from its session and every contacts method answers
`accountNotSupportedByMethod`.

## One store for CardDAV and JMAP

There is no second copy. An AddressBook is a CardDAV address book, a
ContactCard is a vCard stored in one. Cards stay vCard on disk and are turned
into JSContact when read and back into vCard when written, with the
[calcard](https://crates.io/crates/calcard) crate (the RFC 9555 mapping).

- A change over CardDAV (PUT, DELETE, MKCOL, PROPPATCH, deleting an address
  book) shows up in `AddressBook/changes`, `ContactCard/changes` and push.
- A change over JMAP moves the CardDAV sync token, CTag and the card's ETag,
  so phones pick it up with their next sync.
- A card keeps its vCard version: one a phone stored as vCard 3.0 is written
  back as 3.0. Cards made over JMAP are vCard 3.0, which every CardDAV client
  reads.
- What calcard has no JSContact property for (`X-ABSHOWAS`, Apple's grouped
  labels and the like) stays in the card's `vCard` property and goes back into
  the vCard on every write, so a change in the webmail loses nothing a phone
  put there. `ContactCard/get` leaves `vCard` out unless it is asked for by
  name.
- The first address book is made the first time either side looks, with the
  same name ("Kontakte" or "Contacts", after the server's language). The first
  address book of an account is its default; migration 34 makes it so for the
  address books that already exist.

## Ids

| Object | Id |
| --- | --- |
| AddressBook | `b12` |
| ContactCard | `k34` |

## AddressBook

| Property | |
| --- | --- |
| `id`, `name`, `description` | `name` 1–255 bytes, `description` up to 10 000 bytes or `null` |
| `sortOrder` | 0 to 2³¹−1 |
| `isDefault` | exactly one address book is the default |
| `isSubscribed` | always `true` |
| `shareWith` | who else sees it, as for calendars ([jmap-calendars.md](jmap-calendars.md#shared-calendars)): principal ids (`a12`) or addresses of the server to `{ "mayRead": true, "mayWrite": … }` |
| `myRights` | everything `true` for one's own (`mayDelete` is `false` for the only one); for an address book shared with the account `mayWrite` and `mayShare` follow what it was shared with, and `mayDelete` only leaves it |

Address books other people of the server share with the account are listed
next to its own, with their cards in `ContactCard/get`, `/query` and
`/changes`; see [calendars.md](calendars.md).

`AddressBook/get` and `AddressBook/changes` are standard. `AddressBook/set`
creates, changes and destroys address books with the properties above; a
property with only one possible value may be sent with that value. Also:

- `onDestroyRemoveContents`: without it an address book that still holds
  cards is not destroyed (`addressBookHasContents`).
- The only address book cannot be destroyed (`forbidden`). When the default
  one goes, the first of the others becomes the default.
- `onSuccessSetIsDefault` makes an address book the default when everything
  else in the call worked; both address books whose `isDefault` changed are
  reported in `created` or `updated`.

## ContactCard

### ContactCard/get

Standard `/get`. `ids: null` works up to `maxObjectsInGet` cards; for more,
ask `ContactCard/query` for the ids and fetch them in pages.

Without `properties` every property comes back except `vCard`; with
`properties` only those. `id` and `addressBookIds` are always there.

```json
{
  "id": "k7",
  "addressBookIds": { "b1": true },
  "@type": "Card",
  "version": "1.0",
  "uid": "urn:uuid:0f0b3996-60fe-426a-8ece-4a80b37dcccb",
  "name": {
    "components": [
      { "kind": "given", "value": "Nyu" },
      { "kind": "surname", "value": "Katze" }
    ],
    "full": "Nyu Katze"
  },
  "emails": { "e1": { "address": "nyu@example.org", "contexts": { "private": true } } },
  "phones": { "p1": { "number": "+49 170 0000000", "features": { "mobile": true } } },
  "anniversaries": {
    "b": { "kind": "birth", "date": { "@type": "PartialDate", "year": 1990, "month": 5, "day": 17 } }
  },
  "updated": "2026-09-23T09:17:45Z"
}
```

### ContactCard/set

Standard `/set` with `ifInState`.

**create** takes a JSContact Card. `addressBookIds` names exactly one of the
account's address books; without it the card goes into the default one.
`null` at the top is the same as leaving a property out. The server fills in
`@type`, `version`, a `urn:uuid:` `uid` and `created` when they are missing,
sets `updated` to now, and returns what it set in `created` together with
`id`. A `uid` that another card of the same address book already has is
`alreadyExists` (CardDAV wants UIDs unique within an address book; two address
books may hold the same person).

**update** is a JMAP PatchObject applied to the card, which is then written
back as vCard. `addressBookIds` (whole, or `addressBookIds/<id>`) moves the
card to another address book; it keeps its id. `id`, `uid` and `@type` cannot
change. `updated` is set to now and returned in `updated`.

Checked before anything is stored (`invalidProperties` names the property):

- `@type` is `Card`, `uid` is 1–255 bytes without control characters.
- `created` and `updated` are UTCDates, `kind` is a word.
- The entries of `emails`, `phones`, `addresses`, `onlineServices`,
  `organizations`, `titles`, `notes` and `media` are objects; every email has
  an `address`, every phone a `number`.
- `media` gives pictures as `uri`s; a `data:` URI of a `photo` or `logo` is an
  `image/*`, of a `sound` an `audio/*`. `blobId` is not supported for cards.
- The whole card as vCard is at most 1 MiB (`tooLarge`), the CardDAV limit.
- The vCard it becomes has to be one CardDAV accepts and read back to the same
  `uid`.

Everything else in a card is kept as data.

### ContactCard/query

Filters (RFC 9610, 3.3.1): `inAddressBook`, `uid`, `hasMember`, `kind`,
`createdBefore`, `createdAfter`, `updatedBefore`, `updatedAfter`, `text`,
`name`, `name/given`, `name/surname`, `name/surname2`, `nickname`,
`organization`, `email`, `phone`, `onlineService`, `address` and `note`, also
combined with `AND`, `OR` and `NOT`. Texts are matched case-insensitively and
as substrings, so `email: "nyu@"` finds a card while someone types; every word
has to be there, and `"quoted phrases"` as a whole. `name` also looks at the
full name and nicknames; `text` at names, organizations and titles, emails,
phones, online services, addresses and notes. A card without `kind` is an
`individual`; one without `created` or `updated` does not match the date
filters.

Sort by `created`, `updated`, `name/given`, `name/surname` or
`name/surname2`; without a sort, by id. `position` (also negative), `anchor`,
`anchorOffset`, `limit` (at most 5000) and `calculateTotal` work as in
RFC 8620. A query that spends more than five seconds reading cards stops with
`serverUnavailable`.

`ContactCard/queryChanges` works as in RFC 8620 (`canCalculateChanges` is
`true`): every card that changed since `sinceQueryState` is in `removed`, and
those that match now are in `added` at their place.

### ContactCard/changes

Standard. The state is the account's change number, shared with mail and
calendars.

## Push

`AddressBook` and `ContactCard` are push types of the EventSource, next to the
mail and calendar types.

## Not supported

- Principals of their own (`urn:ietf:params:jmap:principals`); principal ids
  are account ids
- More than one address book per card
- `ContactCard/copy` (there is one account per login)
- Pictures as blobs: `media` with `blobId`, and `ContactCard/parse`

## Security

What JMAP Contacts adds was reviewed with the same questions as JMAP Calendars
([security-audit-0.7.0.md](security-audit-0.7.0.md)):

- **Accounts.** Every address book and card id is resolved within the
  signed-in account, and every SQL statement is scoped by `account_id`; a
  foreign id is `notFound`, a foreign `accountId` `accountNotFound`, and an
  address book of another account in `addressBookIds` is `invalidProperties`.
  The tests in `crates/uwumail-jmap/tests/contacts.rs` try each of these.
- **What reaches phones.** Every write goes through calcard both ways and must
  come back as a vCard with the same UID before it is stored, under the CardDAV
  size limit, so JMAP cannot store a card a phone would choke on or that CardDAV
  would have refused.
- **Pictures.** A `data:` URI has to carry the media type its kind needs; an
  HTML or script "photo" is refused. Remote picture URIs are stored as data
  and never fetched by the server; clients decide whether to load them.
- **Work per request.** `/get` with `ids: null` is bounded by
  `maxObjectsInGet`, `/set` by `maxObjectsInSet`, a query by five seconds of
  reading cards, and one card by 1 MiB. Parsing and conversion run off the
  async threads.
- **Switches.** The capability and all methods follow the account's CardDAV
  switch, as JMAP Calendars follows CalDAV's.
