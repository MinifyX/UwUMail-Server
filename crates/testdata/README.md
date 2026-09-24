# Test data

Files the tests of several crates share.

## `smime-signed.eml`

A real S/MIME signed message (`multipart/signed`), from `MIME-Version` down. Tests put their own
`From`, `To` and the like in front of it and check that it comes out of SMTP, IMAP and JMAP byte for
byte: the signature covers the signed part exactly, so any change on the way breaks it.

The signed text holds what servers like to change: lines starting with a dot, a line that is only a
dot, a line starting with `From `, trailing spaces and a tab.

It was signed with a throwaway P-256 key and a self-signed certificate for the made-up
`mini@example.de`; the key was deleted right after. The file is kept as-is by git (`-text` in
`.gitattributes`), since its lines have to end in CRLF. To check it:

```bash
openssl smime -verify -noverify -in crates/testdata/smime-signed.eml -out /dev/null
```
