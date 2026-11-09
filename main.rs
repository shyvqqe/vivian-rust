use std::{
    env,
    error::Error,
    net::Ipv4Addr,
    num::{NonZeroU16, NonZeroU64},
    time::Duration,
};

use axum::{Router, extract::State, http::StatusCode, routing::get};
use serenity::all::{
    Client, Context, CreateCommand, CreateInteractionResponse, CreateInteractionResponseMessage,
    EventHandler, GatewayIntents, GuildId, Http, Interaction, Ready, ShardStageUpdateEvent,
};
use serenity::http::HttpError;
use tokio::{
    net::TcpListener,
    sync::{OnceCell, watch},
    time::timeout,
};

const PRODUCTION_BOT_ID: u64 = 1_553_826_513_474_945_114;
const PING: &str = "ping";
const REST_TIMEOUT: Duration = Duration::from_secs(20);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, PartialEq, Eq)]
enum DiscordStartup {
    Verifying,
    Initializing,
    Connecting,
    Registering,
    RegistrationFailed,
    Ready,
}

struct Prototype {
    guild_id: GuildId,
    registered: OnceCell<()>,
    startup: watch::Sender<DiscordStartup>,
}

#[serenity::async_trait]
impl EventHandler for Prototype {
    async fn shard_stage_update(&self, _: Context, event: ShardStageUpdateEvent) {
        println!("Discord gateway stage: {:?}.", event.new);
    }

    async fn ready(&self, context: Context, ready: Ready) {
        let _ = self.registered
            .get_or_try_init(|| async {
                self.startup.send_replace(DiscordStartup::Registering);
                println!("Discord READY received. Registering /ping in test server {}.", self.guild_id);
                let result = timeout(
                    REST_TIMEOUT,
                    self.guild_id.create_command(
                        &context.http,
                        CreateCommand::new(PING).description("Check if the prototype is online"),
                    ),
                )
                .await;
                let error = match result {
                    Ok(Ok(_)) => None,
                    Ok(Err(error)) => Some(discord_error(&error)),
                    Err(_) => Some("Discord did not respond within 20 seconds.".into()),
                };
                if let Some(error) = error {
                    eprintln!("Could not register /ping: {error} Check the test server ID and app installation.");
                    self.startup.send_replace(DiscordStartup::RegistrationFailed);
                    return Err(());
                }
                println!(
                    "{} is ready in test server {}.",
                    ready.user.name, self.guild_id
                );
                self.startup.send_replace(DiscordStartup::Ready);
                Ok(())
            })
            .await;
    }

    async fn interaction_create(&self, context: Context, interaction: Interaction) {
        let Interaction::Command(command) = interaction else {
            return;
        };
        if command.guild_id != Some(self.guild_id) || command.data.name != PING {
            return;
        }
        let response = CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("Pong! Vivian Rust is online.")
                .ephemeral(true),
        );
        if command
            .create_response(&context.http, response)
            .await
            .is_err()
        {
            eprintln!("Could not respond to /ping.");
        }
    }
}

fn web_routes(startup: watch::Receiver<DiscordStartup>) -> Router {
    Router::new()
        .route("/", get(|| async { "Vivian Rust prototype\n" }))
        .route("/healthz", get(healthz))
        .with_state(startup)
}

async fn healthz(
    State(startup): State<watch::Receiver<DiscordStartup>>,
) -> (StatusCode, &'static str) {
    let body = match *startup.borrow() {
        DiscordStartup::Verifying => "starting: verifying Discord bot token\n",
        DiscordStartup::Initializing => "starting: initializing Discord gateway\n",
        DiscordStartup::Connecting => "starting: waiting for Discord READY\n",
        DiscordStartup::Registering => "starting: registering /ping\n",
        DiscordStartup::RegistrationFailed => "failed: could not register /ping\n",
        DiscordStartup::Ready => return (StatusCode::OK, "ready\n"),
    };
    (StatusCode::SERVICE_UNAVAILABLE, body)
}

fn discord_error(error: &serenity::Error) -> String {
    match error {
        serenity::Error::Http(HttpError::UnsuccessfulRequest(response)) => {
            return match response.status_code.as_u16() {
                401 => "HTTP 401: Discord rejected the bot token. Check VIVIAN_TEST_DISCORD_TOKEN."
                    .into(),
                403 => "HTTP 403: Discord denied access.".into(),
                429 => "HTTP 429: Discord rate limited the request.".into(),
                status => format!("Discord returned HTTP {status}."),
            };
        }
        serenity::Error::Http(HttpError::InvalidHeader(_)) => {
            "The bot token contains invalid characters."
        }
        serenity::Error::Http(HttpError::Request(error)) if error.is_timeout() => {
            "The Discord request timed out."
        }
        serenity::Error::Http(HttpError::Request(error)) if error.is_connect() => {
            "Could not connect to Discord."
        }
        serenity::Error::Http(HttpError::Request(error)) if error.is_decode() => {
            "Discord returned an unreadable response."
        }
        serenity::Error::Json(_) | serenity::Error::Decode(..) => {
            "Discord returned an unreadable response."
        }
        _ => "The Discord request could not be completed.",
    }
    .into()
}

async fn verify_test_bot(http: &Http) -> Result<(), String> {
    let user = http
        .get_current_user()
        .await
        .map_err(|error| format!("Discord bot verification failed: {}", discord_error(&error)))?;
    if !user.bot {
        return Err("Use the token from the Bot tab of the test Discord application.".into());
    }
    if user.id.get() == PRODUCTION_BOT_ID {
        return Err("Use a separate Discord application for the prototype.".into());
    }
    Ok(())
}

async fn run_discord(
    token: &str,
    guild_id: GuildId,
    startup: watch::Sender<DiscordStartup>,
) -> Result<(), Box<dyn Error>> {
    println!("Verifying the Discord test bot token.");
    timeout(REST_TIMEOUT, verify_test_bot(&Http::new(token)))
        .await
        .map_err(|_| "Discord bot verification timed out after 20 seconds.")??;
    startup.send_replace(DiscordStartup::Initializing);
    println!("Discord test bot token verified. Initializing the gateway client.");

    let mut status = startup.subscribe();
    let mut client = timeout(
        REST_TIMEOUT,
        Client::builder(token, GatewayIntents::empty()).event_handler(Prototype {
            guild_id,
            registered: OnceCell::new(),
            startup: startup.clone(),
        }),
    )
    .await
    .map_err(|_| "Discord gateway initialization timed out after 20 seconds.")?
    .map_err(|_| "Could not initialize the Discord client.")?;

    startup.send_replace(DiscordStartup::Connecting);
    println!("Connecting to the Discord gateway.");
    let gateway = client.start();
    tokio::pin!(gateway);
    tokio::select! {
        result = &mut gateway => return gateway_exit(result),
        started = timeout(STARTUP_TIMEOUT, status.wait_for(|phase| {
            matches!(phase, DiscordStartup::Ready | DiscordStartup::RegistrationFailed)
        })) => match started {
            Ok(Ok(phase)) if *phase == DiscordStartup::Ready => {}
            Ok(Ok(_)) => return Err("Initial /ping registration failed. See the preceding Discord error.".into()),
            Ok(Err(_)) => return Err("The Discord startup status channel closed.".into()),
            Err(_) => return Err(match *startup.borrow() {
                DiscordStartup::Registering => "Initial /ping registration did not finish within 60 seconds.",
                _ => "Discord gateway did not send READY within 60 seconds.",
            }.into()),
        }
    }
    gateway_exit(gateway.await)
}

fn gateway_exit(result: serenity::Result<()>) -> Result<(), Box<dyn Error>> {
    match result {
        Ok(()) => Err("The Discord gateway stopped unexpectedly.".into()),
        Err(serenity::Error::Gateway(error)) => {
            Err(format!("The Discord gateway stopped: {error}").into())
        }
        Err(_) => Err("The Discord gateway stopped with an error.".into()),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    if let Err(error) = dotenvy::from_path(".env")
        && !error.not_found()
    {
        return Err("Could not read .env. Check its format.".into());
    }
    let token = env::var("VIVIAN_TEST_DISCORD_TOKEN").unwrap_or_default();
    let token = token.trim();
    if token.is_empty() {
        return Err("Set VIVIAN_TEST_DISCORD_TOKEN for the test Discord application.".into());
    }
    let guild_id = env::var("VIVIAN_TEST_GUILD_ID")
        .unwrap_or_default()
        .trim()
        .parse::<NonZeroU64>()
        .map(GuildId::from)
        .map_err(|_| "Set VIVIAN_TEST_GUILD_ID to the numeric ID of the test server.")?;
    let port = env::var("PORT")
        .unwrap_or_else(|_| "10000".into())
        .parse::<NonZeroU16>()
        .map_err(|_| "Set PORT to a number between 1 and 65535.")?;
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port.get())).await?;
    println!("HTTP listening on 0.0.0.0:{port}.");

    let (startup, status) = watch::channel(DiscordStartup::Verifying);
    let router = web_routes(status);
    tokio::select! {
        result = run_discord(token, guild_id, startup) => result,
        result = axum::serve(listener, router) => result.map_err(Into::into),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use serenity::http::HttpBuilder;
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_waits_for_command_registration() {
        let (startup, status) = watch::channel(DiscordStartup::Verifying);
        let app = web_routes(status);
        let request = || Request::get("/healthz").body(Body::empty()).unwrap();

        let starting = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(starting.status(), StatusCode::SERVICE_UNAVAILABLE);

        startup.send_replace(DiscordStartup::Registering);
        let registering = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(registering.status(), StatusCode::SERVICE_UNAVAILABLE);

        startup.send_replace(DiscordStartup::RegistrationFailed);
        let failed = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(failed.status(), StatusCode::SERVICE_UNAVAILABLE);

        startup.send_replace(DiscordStartup::Ready);
        let running = app.oneshot(request()).await.unwrap();
        assert_eq!(running.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn bot_verification_identifies_unauthorized_and_blocks_production() {
        let cases = [
            (
                StatusCode::UNAUTHORIZED,
                r#"{"code":0,"message":"private response detail"}"#,
                Some("HTTP 401"),
            ),
            (
                StatusCode::OK,
                r#"{"id":"1553826513474945114","username":"Vivian","bot":true}"#,
                Some("separate Discord application"),
            ),
            (
                StatusCode::OK,
                r#"{"id":"100","username":"Vivian Test","bot":true}"#,
                None,
            ),
        ];
        for (status, body, expected_error) in cases {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let address = listener.local_addr().unwrap();
            let app = Router::new().route(
                "/api/v10/users/@me",
                get(move || async move { (status, body) }),
            );
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let http = HttpBuilder::new("test-token")
                .proxy(format!("http://{address}"))
                .ratelimiter_disabled(true)
                .build();
            let result = verify_test_bot(&http).await;
            server.abort();
            match expected_error {
                Some(message) => {
                    let error = result.unwrap_err();
                    assert!(error.contains(message), "{error}");
                    assert!(!error.contains("private response detail"));
                }
                None => assert!(result.is_ok(), "{result:?}"),
            }
        }
    }
}
