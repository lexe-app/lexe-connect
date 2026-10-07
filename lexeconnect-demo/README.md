# LexeConnect demo

A small REQUESTER service for trying LexeConnect end to end.
Each visitor gets a credential request shown as a QR code and link, in the
delivery mode they pick.
Once the WALLET responds, the page shows the connected wallet's client info and
balance, read with the granted credential.
Requests ask for the `read_info` scope on mainnet.

## Run

The WALLET reaches this server over `https://`, so it needs a public url.
For local development, a Cloudflare quick tunnel to port 8000 works:

```bash
cloudflared tunnel --url http://127.0.0.1:8000
# Prints https://<random>.trycloudflare.com
cargo run -- https://<random>.trycloudflare.com
```

Open the url, pick a delivery mode, and scan the QR code with the Lexe app,
or open the page on your phone and tap "Open in Lexe".

## Delivery modes

- **Redirect**: after approving, the phone opens the session's callback url,
  which carries the encrypted response.
- **Post**: the WALLET POSTs the encrypted response to the callback url.
- **Mailbox**: the WALLET POSTs the encrypted response to Lexe's mailbox, which
  this server polls.

Sessions expire after ten minutes.
