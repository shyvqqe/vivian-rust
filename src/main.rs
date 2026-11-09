use std::{
    env,
    error::Error,
    net::Ipv4Addr,
    num::{NonZeroU16, NonZeroU64},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use axum::{Router, extract::State, http::StatusCode, routing::get};
use serenity::all::{
    Client, Context, CreateCommand, CreateInteractionResponse, CreateInteractionResponseMessage,
    EventHandler, GatewayIntents, GuildId, Http, Interaction, Ready,
};
use tokio::{net::TcpListener, sync::OnceCell};

const PRODUCTION_APPLICATION_ID: u64 = 1_553_826_513_474_945_114;
const PING: &str = "ping";

struct Prototype {
    guild_id: GuildId,
    registered: OnceCell<()>,
    ready: Arc<AtomicBool>,
}

#[serenity::async_trait]
impl EventHandler for Prototype {
    async fn ready(&self, context: Context, ready: Ready) {
        let registered = self
            .registered
            .get_or_try_init(|| async {
                self.guild_id
                    .create_command(
                        &context.http,
                        CreateCommand::new(PING).description("Check if the prototype is online"),
                    )
                    .await?;
                self.ready.store(true, Ordering::Relaxed);
                println!(
                    "{} is ready in test server {}.",
                    ready.user.name, self.guild_id
                );
                Ok::<(), serenity::Error>(())
            })
            .await;
        if registered.is_err() {
            eprintln!("Could not register /ping. Check the test server ID and bot installation.");
        }
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

fn web_routes(ready: Arc<AtomicBool>) -> Router {
    Router::new()
        .route("/", get(|| async { "Vivian Rust prototype\n" }))
        .route("/healthz", get(healthz))
        .with_state(ready)
}

async fn healthz(State(ready): State<Arc<AtomicBool>>) -> (StatusCode, &'static str) {
    if ready.load(Ordering::Relaxed) {
        (StatusCode::OK, "ready\n")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "starting\n")
    }
}

async fn run_discord(
    token: &str,
    guild_id: GuildId,
    ready: Arc<AtomicBool>,
) -> Result<(), Box<dyn Error>> {
    let application = Http::new(token)
        .get_current_application_info()
        .await
        .map_err(|_| "Could not verify the test application with Discord. Check the token and connection.")?;
    if application.id.get() == PRODUCTION_APPLICATION_ID {
        return Err("Use a separate Discord application for the prototype.".into());
    }

    let mut client = Client::builder(token, GatewayIntents::empty())
        .event_handler(Prototype {
            guild_id,
            registered: OnceCell::new(),
            ready,
        })
        .await
        .map_err(|_| "Could not initialize the Discord client.")?;
    client
        .start()
        .await
        .map_err(|_| "The Discord gateway stopped with an error.")?;
    Ok(())
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

    let ready = Arc::new(AtomicBool::new(false));
    let router = web_routes(Arc::clone(&ready));
    tokio::select! {
        result = run_discord(token, guild_id, ready) => result,
        result = axum::serve(listener, router) => result.map_err(Into::into),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_waits_for_command_registration() {
        let ready = Arc::new(AtomicBool::new(false));
        let app = web_routes(Arc::clone(&ready));
        let request = || Request::get("/healthz").body(Body::empty()).unwrap();

        let starting = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(starting.status(), StatusCode::SERVICE_UNAVAILABLE);

        ready.store(true, Ordering::Relaxed);
        let running = app.oneshot(request()).await.unwrap();
        assert_eq!(running.status(), StatusCode::OK);
    }
}
