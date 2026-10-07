//! A demo REQUESTER service. Each visit to `/` mints a request and shows it
//! as a QR code and link, in a delivery mode chosen with a toggle. On a grant,
//! the credential is used to show the (mainnet) wallet's client info and
//! balance. Pass this server's public base url, e.g. a tunnel to port 8000.
//!
//! ```bash
//! cargo run -- https://xyz.trycloudflare.com
//! ```

use std::{
    collections::{BTreeSet, HashMap},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{StatusCode, Uri},
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
};
use lexe::{
    anyhow,
    config::{DeployEnv, WalletEnvConfig},
    tracing::{info, warn},
    types::auth::{ClientCredentials, CredentialsRef},
    util::hex,
    wallet::LexeWallet,
};
use lexe_connect::{
    LEXE_MAILBOX_URL,
    http::LexeConnectClient,
    request::{CredentialRequestParams, Delivery},
    requester::{AcceptError, PendingRequest},
    response::{CredentialResponse, CredentialResult},
};
use lexe_crypto::rng::{RngExt, SysRng};
use serde::Deserialize;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    const LISTEN_ADDR: SocketAddr =
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8000);

    lexe::init_logger("info");

    let base_url = std::env::args()
        .nth(1)
        .expect("usage: requester-example <public-base-url>");
    let state = Arc::new(AppState {
        base_url,
        mailbox_client: LexeConnectClient::new(DeployEnv::Prod)?,
        sessions: Mutex::new(HashMap::new()),
    });

    let app = Router::new()
        .route("/", get(new_session))
        .route("/new", get(new_session))
        .route("/s/{id}", get(index))
        .route("/s/{id}/status", get(status))
        .route("/s/{id}/callback", get(redirected).post(posted))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind(LISTEN_ADDR).await?;
    info!("Listening on {LISTEN_ADDR}; open {}/", state.base_url);
    axum::serve(listener, app).await?;
    Ok(())
}

struct AppState {
    base_url: String,
    mailbox_client: LexeConnectClient,
    /// Live sessions by id. Expired ones are evicted as new ones are created.
    sessions: Mutex<HashMap<String, Arc<Session>>>,
}

/// One visitor's request and its outcome. Each has its own callback url, so
/// any number can be in flight at once.
struct Session {
    id: String,
    mode: Mode,
    created_at: Instant,
    pending: PendingRequest,
    /// The accepted response, once one arrives. Later deliveries are refused.
    accepted: Mutex<Option<CredentialResponse>>,
    /// The wallet info fetched with a granted credential, or the error.
    wallet_info: Mutex<Option<Result<String, String>>>,
}

/// How the WALLET delivers the response.
#[derive(Copy, Clone, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    Post,
    Redirect,
    Mailbox,
}

impl AppState {
    /// Sessions older than this are gone, and mailbox polling has stopped.
    const SESSION_TTL: Duration = Duration::from_secs(600);
    /// Bounds memory use from unattended visitors.
    const MAX_SESSIONS: usize = 1000;

    /// Mints a session, or `None` if there are too many live ones.
    fn create_session(self: &Arc<Self>, mode: Mode) -> Option<Arc<Session>> {
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, session| !session.is_expired());
        if sessions.len() >= Self::MAX_SESSIONS {
            return None;
        }
        let session = Session::start(self, mode);
        sessions.insert(session.id.clone(), session.clone());
        Some(session)
    }

    fn session(&self, id: &str) -> Option<Arc<Session>> {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(id).filter(|s| !s.is_expired()).cloned()
    }
}

impl Session {
    /// Mints a request and, with mailbox delivery, polls for its response.
    fn start(state: &Arc<AppState>, mode: Mode) -> Arc<Self> {
        let mut rng = SysRng::new();
        let id = hex::encode(&rng.gen_bytes::<16>());
        let callback_url = format!("{}/s/{id}/callback", state.base_url);
        let delivery = match mode {
            Mode::Post => Delivery::Post(callback_url),
            Mode::Redirect => Delivery::Redirect(callback_url),
            Mode::Mailbox => Delivery::Mailbox(LEXE_MAILBOX_URL.to_owned()),
        };
        let params = CredentialRequestParams {
            delivery,
            account: Some("@example".into()),
            metadata: Some("example-metadata".into()),
            requester_name: None,
            requester_icon: None,
            scopes: ["read_info"].map(String::from).into(),
            permissions: BTreeSet::new(),
            label: Some("Requester example".into()),
            expires_at: None,
        };
        let pending = PendingRequest::new(&mut rng, params)
            .expect("Example params are valid");

        let session = Arc::new(Self {
            id,
            mode,
            created_at: Instant::now(),
            pending,
            accepted: Mutex::new(None),
            wallet_info: Mutex::new(None),
        });
        if mode == Mode::Mailbox {
            let session = session.clone();
            let client = state.mailbox_client.clone();
            tokio::spawn(async move {
                if let Err(err) = session.poll_mailbox(&client).await {
                    let id = &session.id;
                    warn!("[{id}] Mailbox poll failed: {err:#}");
                }
            });
        }
        session
    }

    fn is_expired(&self) -> bool {
        self.created_at.elapsed() >= AppState::SESSION_TTL
    }

    async fn poll_mailbox(
        self: &Arc<Self>,
        client: &LexeConnectClient,
    ) -> anyhow::Result<()> {
        let (mailbox_url, address) =
            self.pending.mailbox().expect("Mailbox delivery");
        let blob = client
            .mailbox_poll(
                mailbox_url,
                &address,
                Duration::from_secs(1),
                AppState::SESSION_TTL,
            )
            .await?;
        let result = self.pending.accept_body(&blob.0);
        let _ = self.accept(result);
        Ok(())
    }

    fn accept(
        self: &Arc<Self>,
        result: Result<CredentialResponse, AcceptError>,
    ) -> Result<(), (StatusCode, String)> {
        let id = &self.id;
        let response = result.map_err(|err| {
            warn!("[{id}] Rejected response: {err:#}");
            (StatusCode::BAD_REQUEST, format!("{err:#}"))
        })?;

        let mut accepted = self.accepted.lock().unwrap();
        if accepted.is_some() {
            let message = "A response was already accepted".to_owned();
            return Err((StatusCode::CONFLICT, message));
        }
        match &response.result {
            CredentialResult::Granted(grant) => {
                info!("[{id}] Accepted response: granted");
                let session = self.clone();
                let credential = grant.credential.clone();
                tokio::spawn(async move {
                    session.load_wallet_info(&credential).await
                });
            }
            CredentialResult::Error(err) => {
                let code = &err.code;
                info!("[{id}] Accepted response: declined: {code:?}");
            }
        }
        *accepted = Some(response);
        Ok(())
    }

    async fn load_wallet_info(&self, credential: &str) {
        let id = &self.id;
        let wallet_info = Self::fetch_wallet_info(credential)
            .await
            .map_err(|err| format!("{err:#}"));
        match &wallet_info {
            Ok(_) => info!("[{id}] Fetched wallet info"),
            Err(err) => warn!("[{id}] Wallet info unavailable: {err}"),
        }
        *self.wallet_info.lock().unwrap() = Some(wallet_info);
    }

    /// Reads the client's info and the wallet's balance as an SDK client,
    /// rendered as HTML.
    async fn fetch_wallet_info(credential: &str) -> anyhow::Result<String> {
        let credentials = ClientCredentials::from_string(credential)?;
        let wallet = LexeWallet::without_db(
            WalletEnvConfig::mainnet(),
            CredentialsRef::ClientCredentials(&credentials),
        )?;
        let client_info = wallet.client_info().await?;
        let client_info =
            escape_html(&lexe::serde_json::to_string_pretty(&client_info)?);
        let node_info = wallet.node_info().await?;
        let lightning_sat = node_info.lightning_balance.sats_u64();
        let onchain_sat = node_info.onchain_balance.sats_u64();
        Ok(format!(
            "<div class=\"stats\">\
               <div><span class=\"value\">{lightning_sat}</span>\
                 <span class=\"label\">sats Lightning</span></div>\
               <div><span class=\"value\">{onchain_sat}</span>\
                 <span class=\"label\">sats on-chain</span></div>\
             </div>\
             <h3>Client info</h3><pre>{client_info}</pre>"
        ))
    }
}

impl Mode {
    const ALL: [Self; 3] = [Self::Post, Self::Redirect, Self::Mailbox];

    fn as_str(self) -> &'static str {
        match self {
            Self::Post => "post",
            Self::Redirect => "redirect",
            Self::Mailbox => "mailbox",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Post => "Post",
            Self::Redirect => "Redirect",
            Self::Mailbox => "Mailbox",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Post => {
                "After approving, the wallet POSTs the response here."
            }
            Self::Redirect => {
                "Meant for app-to-app sharing. After approving, the phone \
                 opens the redirect uri, here this server's callback."
            }
            Self::Mailbox => {
                "After approving, the wallet POSTs the response to Lexe's \
                 mailbox, which this server polls."
            }
        }
    }
}

#[derive(Deserialize)]
struct NewQuery {
    mode: Option<Mode>,
}

/// Mints a session and redirects to its page.
async fn new_session(
    State(state): State<Arc<AppState>>,
    Query(query): Query<NewQuery>,
) -> Response {
    let mode = query.mode.unwrap_or(Mode::Post);
    match state.create_session(mode) {
        Some(session) => {
            Redirect::to(&format!("/s/{}", session.id)).into_response()
        }
        None => (StatusCode::SERVICE_UNAVAILABLE, "Too many sessions")
            .into_response(),
    }
}

/// The session's request as a QR code and link, with a toggle that mints a
/// new session in another mode. Polls the status until the result is final.
async fn index(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(session) = state.session(&id) else {
        return expired_page();
    };

    let modes = Mode::ALL
        .map(|mode| {
            let id = mode.as_str();
            let label = mode.label();
            let active = if mode == session.mode {
                r#" class="active""#
            } else {
                ""
            };
            format!(r#"<a href="/new?mode={id}"{active}>{label}</a>"#)
        })
        .concat();
    let hint = session.mode.hint();
    let connection_string = session.pending.connection_string();

    page(&format!(
        r#"<div id="request">
    <h2>Connect your Lexe wallet</h2>
    <p class="hint">Scan with the Lexe app, or open the link on your phone.</p>
    <nav class="modes">{modes}</nav>
    <p class="hint">{hint}</p>
    <canvas id="qr"></canvas>
    <a class="button" href="{connection_string}">Open in Lexe</a>
    <details>
      <summary>Connection string</summary>
      <div class="copyable">
        <code id="connection-string">{connection_string}</code>
        <button id="copy">Copy</button>
      </div>
    </details>
  </div>
  <div id="result" hidden></div>
<script src="https://cdn.jsdelivr.net/npm/qrcode@1/build/qrcode.min.js"></script>
<script>
  QRCode.toCanvas(document.getElementById("qr"), "{connection_string}",
    {{ width: 256, margin: 0 }});
  const copy = document.getElementById("copy");
  copy.onclick = async () => {{
    const text = document.getElementById("connection-string").textContent;
    await navigator.clipboard.writeText(text);
    copy.textContent = "Copied";
    setTimeout(() => copy.textContent = "Copy", 1500);
  }};
  const check = async () => {{
    const res = await fetch("/s/{id}/status");
    // Expired: reload into the expired page rather than show a stale QR.
    if (res.status === 404) return location.reload();
    if (res.status !== 200) return;
    const html = await res.text();
    document.getElementById("request").hidden = true;
    const result = document.getElementById("result");
    result.innerHTML = html;
    result.hidden = false;
    if (html.includes("data-done")) clearInterval(poll);
  }};
  const poll = setInterval(check, 1000);
  check();
</script>"#
    ))
    .into_response()
}

/// 200 with an HTML summary once a response is accepted, else 204. The
/// summary contains `data-done` once nothing more will change.
async fn status(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(session) = state.session(&id) else {
        return expired();
    };
    let accepted = session.accepted.lock().unwrap();
    let Some(response) = accepted.as_ref() else {
        return StatusCode::NO_CONTENT.into_response();
    };
    let body = match &response.result {
        CredentialResult::Granted(_) => {
            let wallet_info = session.wallet_info.lock().unwrap();
            let wallet_info = match wallet_info.as_ref() {
                None => {
                    "<p class=\"hint\">Fetching wallet info...</p>".to_owned()
                }
                Some(Ok(info)) => format!("<div data-done>{info}</div>"),
                Some(Err(err)) => {
                    let err = escape_html(err);
                    format!(
                        "<p class=\"error\" data-done>\
                         Wallet info unavailable: {err}</p>"
                    )
                }
            };
            format!("<h2 class=\"ok\">Connected</h2>{wallet_info}")
        }
        CredentialResult::Error(err) => {
            let code = escape_html(&format!("{:?}", err.code));
            let message =
                escape_html(err.message.as_deref().unwrap_or_default());
            format!(
                "<h2 class=\"error\" data-done>Declined</h2>\
                 <p class=\"hint\">{code}: {message}</p>"
            )
        }
    };
    Html(body).into_response()
}

/// Redirect delivery: the WALLET sent the user here with `?response=`. Once
/// accepted, the user lands back on the session page to see the result.
async fn redirected(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    uri: Uri,
) -> Response {
    let Some(session) = state.session(&id) else {
        return expired_page();
    };
    // `accept_redirect` wants a full url; the scheme and host don't matter.
    let url = format!("https://localhost{uri}");
    let result = session.pending.accept_redirect(&url);
    match session.accept(result) {
        // A revisited callback just shows the result again.
        Ok(()) | Err((StatusCode::CONFLICT, _)) => {
            Redirect::to(&format!("/s/{id}")).into_response()
        }
        Err((status, message)) => {
            error_page(status, "Response rejected", &message)
        }
    }
}

/// Post delivery: the WALLET posted the response body here.
async fn posted(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let Some(session) = state.session(&id) else {
        return expired();
    };
    let result = session.pending.accept_body(&body);
    session.accept(result).into_response()
}

fn expired() -> Response {
    (StatusCode::NOT_FOUND, "Session expired. Start over at /").into_response()
}

fn expired_page() -> Response {
    let hint = "Sessions last ten minutes.";
    error_page(StatusCode::NOT_FOUND, "Session expired", hint)
}

/// An error card. `title` and `hint` are plain text.
fn error_page(status: StatusCode, title: &str, hint: &str) -> Response {
    let title = escape_html(title);
    let hint = escape_html(hint);
    let body = page(&format!(
        "<h2 class=\"error\">{title}</h2>\
         <p class=\"hint\">{hint}</p>\
         <a class=\"button\" href=\"/\">Start over</a>"
    ));
    (status, body).into_response()
}

/// Wraps card content in the page shell: styling, header, and card.
fn page(content: &str) -> Html<String> {
    const STYLE: &str = include_str!("style.css");
    Html(format!(
        r#"<!doctype html>
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>LexeConnect Demo</title>
<style>{STYLE}</style>
<div class="page">
<header>
  <h1><a href="/">LexeConnect Demo</a></h1>
  <a href="https://github.com/lexe-app/lexe-connect">Read the spec</a>
</header>
<main class="card">
{content}
</main>
</div>
"#
    ))
}

/// Escapes text for HTML element content and quoted attribute values.
fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(c),
        }
    }
    escaped
}
