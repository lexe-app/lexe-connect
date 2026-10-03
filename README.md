# LexeConnect

*Version 1.0.1*
<!-- Author: Max Fang, Lexe Corporation -->

A simple and secure protocol for one-click credential sharing.
Supports app-to-app and app-to-service sharing.
Based on [HPKE], a modern public key encryption standard ([RFC 9180])
co-authored by Cloudflare.

[HPKE]: https://blog.cloudflare.com/hybrid-public-key-encryption/
[RFC 9180]: https://www.rfc-editor.org/rfc/rfc9180.html

## Use cases

### App-to-app credential sharing

**Example:** BillSplit (a mobile app for splitting bills with friends) adds a
"Connect Lexe" button so that BillSplit users can settle what they owe from
their Lexe wallet without having to navigate menus or copy and paste a Lexe
[client credential] string.

[client credential]: https://docs.lexe.tech/authentication/#lexe-client-credentials

**Overview:**

- User taps "Connect Lexe" within BillSplit and is redirected to the Lexe
  mobile app.
- Within the Lexe app, the user approves BillSplit's requested scopes and
  spending budget in one tap, and is redirected back to BillSplit.
- BillSplit receives the user's credential and can now pay friends from the
  user's Lexe wallet, subject to the user's approved scopes and budget.

**Demo:** [video](https://r2.iexe.tech/lexe-connect/2026-09-18-app-to-app-lexe-connect-demo.mp4)

<!-- GitHub strips the <video> tag, so GitHub readers get only the link
above; renderers that allow raw HTML get the inline player. -->
<video controls width="400"
  src="https://r2.iexe.tech/lexe-connect/2026-09-18-app-to-app-lexe-connect-demo.mp4">
</video>

<!--
TODO(maxfangx): Replace this PoC demo with a video of the real, deployed
flow once it ships.
-->

### App-to-service credential sharing

**Example:** Paygate.com (a web app) allows merchants to share a receive-only
Lexe client credential so that Paygate.com can host a checkout page which
accepts Bitcoin e-commerce payments into the merchant's Lexe wallet without
having the ability to spend any funds.

**Overview:**

- Paygate.com displays a QR code which encodes a LexeConnect connection string.
- The merchant scans the QR code with their Lexe app, and is taken to a page
  showing Paygate's connection request. The merchant approves Paygate's
  requested scopes and permissions.

<!--
TODO(maxfangx): Once implemented, this would be a nice place to include a
video which demonstrates this
-->

## Credential request

The app or service initiating a request is the REQUESTER; the Lexe app is
the WALLET.

To request a credential, the REQUESTER constructs a **connection string**:
a url which can be opened as a link, QR-encoded, or pasted into the WALLET.
It is formed by appending query params to the WALLET's **connect url**,
which for Lexe is `https://lexe.app/connect`:

`<connect url>?<params>`

For example:

`https://lexe.app/connect?v=1&redirect_uri=<uri>&ephemeral_hpke_pubkey=<pubkey>&one_time_secret=<secret>&scopes=read_info,read_payments,receive`

Request and response fields are typed as JSON values. As query params,
each field is rendered as a string and MUST be [percent-encoded] (not
form-encoded, so `+` is literal): strings render as-is, numbers render
in base-10, and arrays render as comma-separated lists, e.g.
`scopes=read_info,read_payments,receive`.

The full reasoning behind the requirements below is in
[Security Analysis](#security-analysis).

### Protocol params

These params are common to any WALLET implementing this protocol.

| Param | Description |
|---|---|
| `v` | **Unsigned integer**, required. The protocol version; currently `1`. The WALLET MUST reject requests with an unrecognized version, but drop unrecognized params, so new params can be added without a version bump. |
| `redirect_uri` | **String**, set exactly one of `redirect_uri`, `post_url`, or `mailbox_url`. Where the user is redirected after approving (or rejecting) the request. MAY be any valid uri, but SHOULD be a `https://` url registered as an [Android App Link] or [iOS Universal Link]. Any other uri can be claimed by other installed apps or local processes, which capture only ciphertext (see [Security Analysis](#security-analysis)). |
| `post_url` | **String**, set exactly one of `redirect_uri`, `post_url`, or `mailbox_url`. The WALLET POSTs the response to this url. MUST be `https://`. See [`post_url` delivery](#post_url-delivery). |
| `mailbox_url` | **String**, set exactly one of `redirect_uri`, `post_url`, or `mailbox_url`. The url of a mailbox which relays the response blob to the REQUESTER. MUST be `https://`. See [`mailbox_url` delivery](#mailbox_url-delivery). |
| `ephemeral_hpke_pubkey` | **String**, required if `redirect_uri` or `mailbox_url` is set. An x25519 public key, encoded as exactly 64 lowercase hex characters. The response is HPKE-encrypted under it (see [Encryption](#encryption)). MUST be freshly generated per request, and is recommended even with `post_url`. |
| `one_time_secret` | **String**, required. A random per-request secret of exactly 32 lowercase hex characters, echoed back in the response. The REQUESTER MUST match it against an outstanding request, and MUST reject reuse. Outstanding requests SHOULD expire after a short window. |
| `account` | **String**, optional but recommended. Lets the user check which REQUESTER account they are connecting their wallet to. MUST uniquely identify the account in a form the user will recognize, e.g. `@janedoe` or `janedoe@gmail.com`. At most 64 UTF-8 bytes. Shown on the approval screen and bound into the HPKE `aad`; see [Request forwarding](#request-forwarding). |
| `metadata` | **String**, optional, at most 1024 UTF-8 bytes. Echoed back verbatim in the response. The REQUESTER may want to base64url encode a JSON blob here. |

REQUESTERs MAY additionally pass `requester_name` (string) and
`requester_icon` (`https://` url), displayed only for verified
REQUESTERs; see [User Approval](#user-approval).

### Credential params

These params are specific to Lexe, so a WALLET adopting this protocol
most likely defines its own. Each has a **grant class**:

- **exact**: read-only on the approval screen. The WALLET MUST NOT let the
  user change it, so the user either grants it exactly as requested or
  rejects the whole request.
- **prefill**: only prefills the field's value on the approval screen,
  which the user may freely change.

| Param | Grant | Description |
|---|---|---|
| `scopes` | exact | **Array of strings**, optional. The requested [scopes], e.g. `read_info`, `read_payments`, `receive`. At least one scope or permission MUST be requested. |
| `permissions` | exact | **Array of strings**, optional. Any explicitly requested fine-grained [permissions]. At least one scope or permission MUST be requested. |
| `label` | prefill | **String**, optional. A suggested [label] for this credential. If unset, the WALLET chooses the prefill, e.g. the verified receiving domain. |
| `expires_at` | prefill | **Unsigned integer**, optional. A suggested expiration time for the credential, in milliseconds since the UNIX epoch. If unset, the WALLET chooses the prefill, e.g. one year, or no expiration for spending credentials with a budget attached. |
| `budget_limit`\* | prefill | **String**, optional. A suggested budget limit, denominated in `budget_currency`. Serialized as a base-10 decimal string. |
| `budget_currency`\* | exact | **String**, required if `budget_limit` is set. The unit the budget is denominated in: `sat` for bitcoin, or a lowercased ISO 4217 code (e.g. `usd`, `eur`). |
| `budget_period`\* | exact | **String**, optional. How frequently the budget should reset. Options: `day`, `week`, `month`, or `never`. |
| `budget_period_multiple`\* | exact | **Unsigned integer**, optional. The multiple of `budget_period` between resets, e.g. the 5 in "every 5 days". Defaults to 1. MUST NOT be set if `budget_period` is `never`. |
| `budget_first_reset`\* | exact | **Unsigned integer**, optional. Time of the first budget reset, in milliseconds since the UNIX epoch. MUST be within one period of the approval time. MUST NOT be set if `budget_period` is `never`. |
| `budget_utc_offset_secs`\* | exact | **Signed integer**, optional. The timezone in which budget resets are computed, only relevant when `budget_period` is `month`. Expressed as a UTC offset in seconds: positive east of UTC, negative west. MUST be within ±14 hours. |

Budget params the REQUESTER leaves unset are chosen by the WALLET and
editable by the user.

\* Planned. Budgets are not yet implemented, so Lexe rejects requests
that set any of these params, and their definitions may change before
release.

[scopes]: https://rust.lexe.tech/types/auth/enum.scope
[permissions]: https://rust.lexe.tech/types/command/struct.createclientrequest#structfield.permissions
[label]: https://rust.lexe.tech/types/command/struct.createclientrequest#structfield.label
[Android App Link]: https://developer.android.com/training/app-links
[iOS Universal Link]: https://developer.apple.com/ios/universal-links/
[percent-encoded]: https://developer.mozilla.org/en-US/docs/Glossary/Percent-encoding

## User Approval

The user enters the WALLET's approval screen in one of three ways:

- Scans a QR code which encodes the connection string (app-to-service).
- Pastes the connection string into the WALLET (app-to-service).
- Is redirected to the WALLET's connect url by the REQUESTER
  (app-to-app).

The WALLET MUST register its connect url as an [Android App Link] and
[iOS Universal Link], so that in the redirect case, the request is
guaranteed to open in the genuine WALLET rather than the browser or an
impostor app.

An example approval screen:

> **billsplit.com wants to connect to your Lexe wallet.**
>
> Account: `@janedoe`
>
> Scopes: `read_info`, `read_payments`, `receive`, `spend`
>
> Budget: $20 / month _(tap to edit)_
>
> Expires: in 1 year _(tap to edit)_
>
> Label: BillSplit App _(tap to edit)_
>
> [ Reject ] [ Approve ]

- To prevent phishing, the headline MUST highlight the receiving domain
  of the `redirect_uri` or `post_url`. If `mailbox_url` is set, or
  `redirect_uri` is not `https://`, no receiving domain is known, so
  the headline SHOULD read "An unverified app", appending the uri's
  scheme and host when `redirect_uri` is set, e.g. "An unverified app
  (`myprotocol://`)". The rest of the uri is chosen by the REQUESTER, so it
  SHOULD NOT be displayed.
- The WALLET MUST show the requested scopes, permissions, and budget;
  the presentation is left to the WALLET.
- If `account` is set, the WALLET MUST display it as the REQUESTER's
  account, e.g. Account: `janedoe@gmail.com`;
  see [Request forwarding](#request-forwarding).
- For spending credentials, the approval screen SHOULD warn the user to
  approve only connections they initiated themselves; see
  [Request forwarding](#request-forwarding).
- The REQUESTER MAY include `requester_name` (string) and `requester_icon`
  (`https://` url) params in the connection string. These are chosen by
  the REQUESTER itself, and a malicious REQUESTER would simply pass the
  branding of the app it is impersonating, so the WALLET MUST NOT display
  them unless the REQUESTER's identity has been verified out-of-band,
  e.g. via a whitelist or an automated domain-verified registration flow,
  similar to [OAuth Dynamic Client Registration].

[OAuth Dynamic Client Registration]: https://www.rfc-editor.org/rfc/rfc7591.html

## Credential response

Once the user approves (or rejects) the request, the WALLET builds a
response and delivers it via the `redirect_uri`, `post_url`, or
`mailbox_url` from the request. If the request itself is invalid, e.g. a
malformed param or an unrecognized version, the WALLET shows the user an
error and delivers no response; the REQUESTER's request expiry covers
this case.

The response is a single JSON object in one of two variants: success,
containing `credential`, or error, containing `error`. The REQUESTER
MUST ignore unrecognized fields, so new fields can be added without a
version bump.

Approval is all-or-nothing: a granted credential MUST carry the
requested `exact` params (e.g. `scopes` and `permissions`) unchanged;
otherwise the WALLET MUST return `error`.

### Protocol fields

These fields are common to any WALLET implementing this protocol.

| Field | Description |
|---|---|
| `credential` | **String**. The granted [client credential]. Returned on success. |
| `error` | **String**. An error code: `user_rejected` if the user rejected the request, otherwise `other`. The REQUESTER MUST treat unrecognized codes as `other`, so new codes can be added in the future. |
| `error_message` | **String**, optional. A human-readable description of the error. Only returned alongside `error`. |
| `one_time_secret` | **String**. The request's `one_time_secret`, echoed back verbatim. Returned on both success and error. |
| `account` | **String**. The request's `account`, echoed back verbatim if set in the request. Returned on both success and error. The REQUESTER MUST reject a response whose value differs from its request. |
| `metadata` | **String**. The request's `metadata`, echoed back verbatim if set in the request. Returned on both success and error. |

### Credential fields

These fields echo the granted values of the
[credential params](#credential-params), so a WALLET adopting this
protocol echoes its own. All are returned only on success; the
`budget_*` fields only if a budget is attached.

| Field | Description |
|---|---|
| `scopes` | **Array of strings**. The granted [scopes], exactly as requested. |
| `permissions` | **Array of strings**. The granted fine-grained [permissions], exactly as requested. |
| `expires_at` | **Unsigned integer**. Expiration time of the granted credential, in milliseconds since the UNIX epoch. Not returned if the credential never expires. |
| `budget_limit`\* | **String**. The approved budget limit, denominated in `budget_currency`. Serialized as a base-10 decimal string. |
| `budget_currency`\* | **String**. The unit the budget is denominated in: `sat` for bitcoin, or a lowercased ISO 4217 code (e.g. `usd`, `eur`). |
| `budget_period`\* | **String**. How frequently the budget resets: `day`, `week`, `month`, or `never`. |
| `budget_period_multiple`\* | **Unsigned integer**. The multiple of `budget_period` between resets, e.g. the 5 in "every 5 days". Not returned if `budget_period` is `never`. |
| `budget_first_reset`\* | **Unsigned integer**. Time of the first budget reset, in milliseconds since the UNIX epoch. Not returned if `budget_period` is `never`. |
| `budget_utc_offset_secs`\* | **Signed integer**. The timezone in which budget resets are computed, only relevant when `budget_period` is `month`. Expressed as a UTC offset in seconds: positive east of UTC, negative west. |

\* Planned. Not returned until budgets are implemented; definitions may
change before release.

A successful response:

```json
{
    "credential": "<client credential>",
    "one_time_secret": "<secret echoed from the request>",
    "account": "@janedoe",
    "metadata": "<metadata echoed from the request>",
    "scopes": ["read_info", "read_payments",  "receive", "spend"],
    "permissions": ["cancel_payment"],
    "expires_at": 1821484800000,
    "budget_limit": "20",
    "budget_currency": "usd",
    "budget_period": "month",
    "budget_period_multiple": 1,
    "budget_first_reset": 1790838000000,
    "budget_utc_offset_secs": -25200
}
```

A rejected request:

```json
{
    "error": "user_rejected",
    "error_message": "<human-readable message>",
    "one_time_secret": "<secret echoed from the request>",
    "account": "@janedoe",
    "metadata": "<metadata echoed from the request>"
}
```

### Encryption

If the request set `ephemeral_hpke_pubkey`, the WALLET encrypts the
response JSON, errors included, into a single binary **blob**: the
32-byte encapsulated key followed by the ciphertext, produced with
single-shot HPKE ([RFC 9180]) using:

| HPKE parameter | Value |
|---|---|
| Mode | Base |
| KEM | DHKEM(X25519, HKDF-SHA256) |
| KDF | HKDF-SHA256 |
| AEAD | ChaCha20-Poly1305 |
| `info` | `domain_separator \|\| one_time_secret_bytes`. `domain_separator` is the ASCII bytes of the WALLET's **domain separator**, which for Lexe is `LexeConnect-v1`; `one_time_secret_bytes` is the request's `one_time_secret` decoded to its 16 raw bytes. |
| `aad` | The request's `account` as UTF-8 bytes, or empty if unset. |

A WALLET adopting this protocol SHOULD choose its own domain separator.

The blob is carried as raw bytes, except in a `redirect_uri` query
param, which base64url encodes it without padding.

### `redirect_uri` delivery

The WALLET redirects the user to `redirect_uri`, preserving any existing
query params and appending the base64url-encoded blob as a single
`response` param. For example:

`https://billsplit.com/callback?existing=param&response=<base64url blob>`

The `redirect_uri` MUST NOT contain a fragment (a trailing `#...`).

### `post_url` delivery

The WALLET POSTs the response to `post_url`. If the request set
`ephemeral_hpke_pubkey`, the body is the raw blob bytes, with
`Content-Type: application/octet-stream`. Otherwise, the body is the
response JSON, with `Content-Type: application/json`. For example, a
plaintext response:

```http
POST https://paygate.com/lexe-connect?existing=param
Content-Type: application/json

{ "credential": "<client credential>", "one_time_secret": "<secret>", ... }
```

The WALLET MUST NOT follow redirects on this POST, otherwise a
redirecting server could re-route a plaintext response to another host
or to a plain `http://` url.

### `mailbox_url` delivery

A mailbox relays the response blob from the WALLET to REQUESTERs which
cannot receive a redirect or run a public server, e.g. desktop and CLI
apps. `ephemeral_hpke_pubkey` is required, so the mailbox holds only
ciphertext (see [Security Analysis](#security-analysis)).

The blob is deposited at an **address**, encoded as 64 lowercase hex
characters:

`address = SHA256(info)`

`info` is the HPKE `info` defined in [Encryption](#encryption). Only
the holders of the connection string can compute the address.

- The WALLET POSTs the raw blob bytes to `<mailbox_url>?address=<address>`
  with `Content-Type: application/octet-stream`. The mailbox responds
  `200 OK` once the blob is stored.
- The REQUESTER polls the same url with GET requests. The mailbox
  responds `404 Not Found` until the blob arrives, then `200 OK` with
  the raw blob bytes as the body.
- The mailbox MUST reject POSTs to an occupied address with
  `409 Conflict`, so the first write wins. A WALLET that misses the
  `200 OK` needs to retry, so the mailbox MUST accept a byte-identical
  re-post with `200 OK`, and SHOULD NOT extend the TTL. Blobs persist
  until TTL expiry, surviving reads, so the REQUESTER can safely retry.
  Responses are a small JSON object plus constant HPKE overhead, so the
  mailbox MAY cap blob size.
- The mailbox SHOULD allow cross-origin GETs, e.g. with
  `Access-Control-Allow-Origin: *`, so browser-based REQUESTERs can poll
  it.

Lexe runs a public mailbox at `https://lexe.app/mailbox` with a 5 minute
TTL. REQUESTERs are welcome to run their own.

## Security Analysis

### Request hijacking

LexeConnect intentionally does not allow a custom scheme (e.g.
`lexeconnect://`) for credential requests: a malicious app could
register itself as a handler for the scheme, intercept the request, and
substitute its own `ephemeral_hpke_pubkey` and `redirect_uri` before
forwarding it on. Requests are therefore delivered only via the connect
url's verified app link, QR scan, or paste.

### Response interception (`redirect_uri`)

The WALLET cannot verify that a `https://` `redirect_uri` is registered as an
app link, and an unclaimed link opens in the browser, leaking its contents
into history, extensions, and sync. With a custom scheme, any installed app
can register itself as a handler and capture the redirect outright. So the
protocol assumes the redirect can be intercepted, and requires
`ephemeral_hpke_pubkey` whenever `redirect_uri` is set. A hijacker then
captures only an opaque blob.

With `post_url`, TLS already protects the response in transit, so
encryption is recommended but not required. The plaintext option exists
to minimize integration cost: a REQUESTER can support LexeConnect with
nothing but an HTTPS endpoint receiving JSON, and no crypto code.

### Response forgery

Anyone can invoke a `redirect_uri` or POST to a `post_url`, so delivery
alone does not prove a response came from the WALLET or matches any
outstanding request. An attacker could thus submit their own valid
credential with a victim's `metadata`, binding the attacker's wallet to
the victim's account, so invoices the victim generates would pay the
attacker.

Every response therefore echoes the request's `one_time_secret`, which the
REQUESTER MUST match against an outstanding request and consume. In a
plaintext `post_url` response, this echo is all that stops a forged
`error` (a classic CSRF) from tricking the REQUESTER into abandoning a
live request. In an encrypted response, the echo sits inside the
ciphertext, so forging any response requires the request's
`ephemeral_hpke_pubkey` and `one_time_secret`. The pubkey MUST be
fresh per request, otherwise an attacker could encrypt their own
credential under a previously seen pubkey.

These bindings assume the connection string stays private until the
request completes: it contains the secrets that responses are checked
against, so an attacker who reads it off the user's screen or clipboard
(e.g. photographs a displayed QR code) can bind their own credential in
the victim's place. Screen and clipboard privacy are outside this
threat model, though the REQUESTER's screen advances before the
intended user has scanned, so the swap can be noticed. To make a
foreign credential evident, REQUESTERs SHOULD display the connected
wallet's identity, such as its Lightning Address or Human Bitcoin
Address, for example: "Connected wallet: `janedoe@lexe.app`".

Responses only match outstanding requests, so the REQUESTER SHOULD also
expire them after a short window, discarding the ephemeral private key
and `one_time_secret`. An expired connection string is then useless,
photographed or not.

### Request forwarding

An attacker can initiate a connection on a service themselves and forward
the real link or QR code to a victim, whose approval screen shows the
service's real domain. Approving would bind the victim's wallet to the
attacker's account, the analog of OAuth device-code phishing.

`account` defends against this. The WALLET displays it, so a
forwarded request visibly names the attacker's account, and binds it into
the HPKE `aad`, so rewriting it to the victim's name yields a response the
attacker cannot decrypt. Plaintext `post_url` responses have no `aad`, so
the WALLET echoes `account` instead, and the REQUESTER MUST reject a
mismatch. Short request expiry and the approval screen warning
([User Approval](#user-approval)) add defense in depth.

### Untrusted mailbox

Mailbox delivery is always encrypted, so the operator learns nothing but
timing and size. A REQUESTER can run its own mailbox for maximum privacy.
The address is a hash, so it reveals nothing about the
`one_time_secret`, and no one without the connection string can find or
occupy it. The short TTL bounds how long an unclaimed blob sits in the
mailbox.

## WALLET-defined parts

A WALLET adopting this protocol only needs to define:

- **Connect url**, registered as an app link. Lexe's is
  `https://lexe.app/connect`.
- **Domain separator**. Lexe's is `LexeConnect-v1`.
- **Credential**, the artifact granted on success. Lexe's is a
  [client credential].
- **Credential params** and their grant classes, echoed in the
  response's [credential fields](#credential-fields). Lexe's are in
  [Credential params](#credential-params).

A mailbox is chosen by the REQUESTER rather than the WALLET, and Lexe's
public mailbox holds only ciphertext, so an adopting WALLET's users can
use it too.

## Test vectors

These vectors check an implementation of [Encryption](#encryption).
Key pairs are derived as in the [RFC 9180] test vectors:

- The REQUESTER's ephemeral key pair is `DeriveKeyPair(ikmR)`, where `ikmR`
  is the bytes `0x00..=0x1f`.
- The WALLET's ephemeral sender key pair is `DeriveKeyPair(ikmE)`, where
  `ikmE` is the bytes `0x20..=0x3f`.

Request:

```
https://lexe.app/connect?v=1&redirect_uri=https%3A%2F%2Fbillsplit.com%2Fcb&ephemeral_hpke_pubkey=b1f1b840de7a3241b02748cf9b05b74dc8c5e8451298738817bd76aa8ebe8c2b&one_time_secret=000102030405060708090a0b0c0d0e0f&account=%40janedoe&scopes=read_info,receive
```

| | Hex |
|---|---|
| `ikmR` | `000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f` |
| `ephemeral_hpke_pubkey` (`pkR`) | `b1f1b840de7a3241b02748cf9b05b74dc8c5e8451298738817bd76aa8ebe8c2b` |
| `ikmE` | `202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f` |
| `info` | `4c657865436f6e6e6563742d7631000102030405060708090a0b0c0d0e0f` |
| `aad` | `406a616e65646f65` |

The plaintext is the exact bytes the WALLET encrypts, so field order
matters here even though JSON does not.

Granted plaintext:

```json
{"credential":"<client credential>","one_time_secret":"000102030405060708090a0b0c0d0e0f","account":"@janedoe","scopes":["read_info","receive"],"permissions":[],"expires_at":1821484800000}
```

Granted blob:

```
693658254630f73ad8da78fb331bf976cd42f90e0e9c9e83f40c51072a6f741718c87dfe078291112ed11fbd29fea41f7e497c02a693481c499717cb9629bf33148f84ebf928eb111ed0525e2d2cc20f23ed8043bba85f5bc6eed0b463e41461d1fc4b8238e0c59aea1e89b6041ebb0718ea5f8133994b74cd6aa22056e9e1827a5daac94185756c7a53dc4ab5d3c3f68aef2293063d337e3726489013a06e27ae68a35c9f8ae7b7fa819dafd8298949878936df24a556c2d92cd08241456aa0a7fc06f1d0562c0d0757c368b873f320f167487a45d80076451cf9135074fd7a81e94dde3e7f9ece9c5676
```

Error plaintext:

```json
{"error":"user_rejected","one_time_secret":"000102030405060708090a0b0c0d0e0f","account":"@janedoe"}
```

Error blob:

```
693658254630f73ad8da78fb331bf976cd42f90e0e9c9e83f40c51072a6f741718c87bfe1089865d609a0ba26eb6d951784f7004bc820c5d17d01cc09d02a23b15d4f9b4be24f711358601117843815b70add504a9a64d5ec6e8d1b361ec1768d5ad4ed03eb3c2cee24b80e04702fb5618b954c438dd0f6cc308a9225be3f0836b5dedff05a6d01574bfc9c06ccb2be20b0139
```

## License

This document is licensed under [CC-BY 4.0]. Any mention of "LexeConnect",
with a link to this document where practicable, satisfies the attribution
requirement.

[CC-BY 4.0]: https://creativecommons.org/licenses/by/4.0/

<!--
## Notes for spec editors

### Why `requester_name` and `requester_icon` are not in the params table

The WALLET only displays them for whitelisted REQUESTERs, so for most
integrators they do nothing. They are instead defined in
[User Approval](#user-approval), right next to the discussion of the phishing
dangers. This way, these params are discoverable primarily by those who have
done a close reading of the spec, taking security into consideration.

### Why only the budget limit is a prefill

The budget's schedule (every `budget_*` param but `budget_limit`) is the
REQUESTER's to fix, e.g. to match its billing. The limit stays the user's
call, so no REQUESTER can make a large limit the price of using its
service. The response echoes the granted `budget_limit`, so a
subscription that needs a minimum limit can decline a credential below
it.

### Future policy knobs

Future policy params could make grant classes negotiable, e.g. for
subscriptions that need a minimum budget:

- `scopes_policy`: `exact` | `at_least` | `any`, default `exact`. Covers
  scopes and permissions jointly. `at_least` = the user may add but not
  remove; `any` = the user picks freely.
- `budget_limit_policy`: `exact` | `at_least` | `any`, default `any`.

Both defaults reproduce today's behavior, so the knobs can be added
without a version bump.

The response echoes the granted `scopes` and `permissions` so the
REQUESTER can verify the grant matches its request without retaining the
request or issuing a client-info call. Under a future `scopes_policy`,
the echo reports a grant that may legitimately differ.

### Why the HPKE `info` binds only the `one_time_secret`

An earlier draft hashed the exact request query string bytes into the
`info`, binding the ciphertext to the full request. Decryption then
required recovering those exact bytes, so any re-encoding by an
intermediary broke it, and the REQUESTER had to store the emitted
string byte-for-byte. Meanwhile the pubkey is fresh per request, so the
ciphertext is already bound to its request, and the REQUESTER verifies
the grant via the fields echoed inside the AEAD. Thus the binding added
fragility without adding security. Carrying the query params verbatim
in the `info`, e.g. base64url encoded, would be equally fragile;
hashing a canonical re-serialization of the request would still add no
security, and canonical JSON is difficult to implement consistently.

The `one_time_secret` stays in the `info` so that even if a keypair is
reused across requests, violating the freshness MUST, each ciphertext
remains bound to its one request.

### Why the response carries no `v`

The REQUESTER first matches a response to its outstanding request by
`one_time_secret`, and that request records the `v` it chose. The WALLET
rejects unrecognized versions without responding, so a response is always
at its request's version, and an echoed `v` would only add a mismatch
case with nothing to protect against.

### Prior Art

- OAuth 2.0
- <https://github.com/ntheile/nwc-wake-spec/blob/master/nwc-wake-spec.md>
- <https://docs.uma.me/uma-auth/introduction>
- <https://openid.net/specs/openid-4-verifiable-presentations-1_0.html>
- <https://github.com/nostr-protocol/nips/pull/1818>
- ISO 18013-7 Annex C
-->
