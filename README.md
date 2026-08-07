<div align="center">

# mail

![version](https://img.shields.io/badge/version-0.1.1-blue) ![crate](https://img.shields.io/badge/crate-fe2o3--mail-orange) ![license](https://img.shields.io/badge/license-Unlicense-green) ![Stay Amazing](https://img.shields.io/badge/Stay-Amazing-important)

Email plumbing for the [Fe₂O₃](https://github.com/isene/fe2o3) suite.

</div>

The parts of reading mail that are pure logic, so the desktop
([kastrup](https://github.com/isene/kastrup)) and the phone
([nomad](https://github.com/isene/nomad)) share one implementation
instead of earning the same bugs twice.

No I/O, no platform APIs, no terminal escapes. Give it bytes, get back text.

## What is in it

| Module | What it does |
|---|---|
| `mime` | Quoted-printable, base64, RFC 2047 header words, latin-1 rescue, and the multipart walk that finds the readable part among the alternatives, attachments and calendar invites |
| `html` | HTML mail reduced to text — tables to markdown, entities decoded, invisible formatting characters dropped |
| `read_state` | Which messages have been read, merged across devices |

```rust
let text = mail::body_text(&raw_body, msg.html_content.as_deref());
```

## Read state

Storage is one file per device in a shared folder, keyed by RFC822
`Message-ID` — the one identity a mail keeps across a maildir, an IMAP
server and a phone. Each device writes only its own file and reads them
all, so there is never a second writer for a sync tool to leave a
conflict copy of.

The rule is asymmetric on purpose, because the laptop is the
authoritative device:

| Event | Effect |
|---|---|
| Read on the laptop | Read everywhere |
| Anything on a phone | Stays on that phone |

That falls out of what each side *writes*, not from a special case in
the merge: the laptop publishes, a phone reads and keeps its own
decisions local. This module is only the merge, and does not know which
side it is running on — newest timestamp wins, whoever wrote it.

Note what is **not** used: the server's `\Seen` flag. Mail arrives here
through a fetcher that never writes flags back, so the server has no
opinion to consult.

Unread is a state, not an absence — marking something unread stores
`read: false` with a fresh timestamp. Dropping the entry would let the
other device's older "read" win the next merge, and the message would
quietly go read again.

## Presentation stays out

A `text/calendar` part comes back as raw iCalendar. If you want an
invite rendered for a human, pass your own renderer:

```rust
let text = mail::mime::extract_mime_text_with(&raw, &|ical| my_invite_view(ical));
```

How an invite should look depends on the display and on taste, so it
belongs to the caller. Same reason there are no colours in here.

## Whole messages, or just bodies

Both work. Give it a body whose encoding a caller already knows, or the
whole RFC822 message straight off an IMAP socket — headers, one part, no
boundary in sight — and it decodes either.

That second shape used to come back blank: the `Content-Type` header made
it look like MIME, the multipart walk found no boundary and gave up, and
nothing else took a turn.

## Two rules that will surprise you

Both are deliberate, both are pinned by tests:

- A **base64 body is sniffed before quoted-printable**, because base64
  ends in `==` and would otherwise be read as a hex escape.
- A **text part of three lines or fewer, alongside an HTML
  alternative, is taken for a stub** — the "this message is in HTML"
  placeholder — and the HTML wins. Right for the mail that motivated
  it; it does mean a genuinely terse plain part gets passed over.

## License

Public domain. See [Unlicense](LICENSE).
