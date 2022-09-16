use async_std::stream::StreamExt;
use futures_util::sink::SinkExt;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Debug)]
pub struct OpenConnectionsResponse {
    pub ok: bool,
    pub url: Option<String>,
    pub error: Option<String>,
}
pub async fn open_connections(token: &str) -> surf::Result<OpenConnectionsResponse> {
    surf::post("https://slack.com/api/apps.connections.open")
        .header(
            surf::http::headers::AUTHORIZATION,
            format!("Bearer {}", token),
        )
        .recv_json()
        .await
}

struct SlackClient {
    token: String,
}

impl SlackClient {
    pub async fn send_message(&self, channel: &str, text: &str) -> surf::Result<()> {
        surf::post("https://slack.com/api/chat.postMessage")
            .header(
                surf::http::headers::AUTHORIZATION,
                format!("Bearer {}", self.token),
            )
            .header(
                surf::http::headers::CONTENT_TYPE,
                "application/json; charset=utf-8",
            )
            .body_json(&serde_json::json!({
                "channel": channel,
                "text": text,
            }))?
            .recv_string()
            .await?;
        Ok(())
    }
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum SocketModeMessage<'s> {
    Hello {},
    Disconnect { reason: &'s str },
    EventsApi { envelope_id: &'s str },
}

#[derive(Serialize)]
pub struct SocketModeAcknowledgeMessage<'s> {
    pub envelope_id: &'s str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<&'s str>,
}

struct RawConfig {
    app_level_token: String,
    user_oauth_token: String,
}

impl RawConfig {
    pub fn from_env() -> Self {
        let app_level_token_key = "SLACK_APP_LEVEL_TOKEN";
        let user_oauth_token_key = "SLACK_USER_OAUTH_TOKEN";
        Self {
            app_level_token: std::env::var("SLACK_APP_LEVEL_TOKEN").expect(&format!(
                "Please set the environment variable {}",
                app_level_token_key
            )),
            user_oauth_token: std::env::var("SLACK_USER_OAUTH_TOKEN").expect(&format!(
                "Please set the environment variable {}",
                user_oauth_token_key
            )),
        }
    }
}

#[async_std::main]
async fn main() {
    let config = RawConfig::from_env();
    let slack_client = SlackClient {
        token: config.user_oauth_token,
    };

    let con_result = open_connections(config.app_level_token.as_str())
        .await
        .expect("Failed to request apps.connections.open");
    if !con_result.ok {
        panic!(
            "app.connections.open failed: {}",
            con_result.error.as_deref().unwrap_or("Unknown error")
        );
    }
    let wss_url = con_result.url.expect("no url passed from server");
    let url = url::Url::parse(&wss_url).expect("Failed to parse entrypoint url");
    let domain = url.domain().expect("no domain name?");
    let tcp_stream = async_std::net::TcpStream::connect(&format!("{}:443", domain))
        .await
        .expect("Failed to connect tcp stream");
    let enc_stream = async_tls::TlsConnector::default()
        .connect(domain, tcp_stream)
        .await
        .expect("Failed to connect encrypted stream");
    let (mut stream, _) = async_tungstenite::client_async(wss_url, enc_stream)
        .await
        .expect("Failed to connect websocket");

    while let Some(m) = stream.next().await {
        match m.expect("Failed to decode websocket frame") {
            tungstenite::Message::Text(t) => match serde_json::from_str(&t) {
                Ok(SocketModeMessage::Hello { .. }) => {
                    println!("Hello: {}", t);
                }
                Ok(SocketModeMessage::Disconnect { reason, .. }) => {
                    println!("Disconnect request: {}", reason);
                    break;
                }
                Ok(SocketModeMessage::EventsApi { envelope_id, .. }) => {
                    println!("Events API Message: {}", t);
                    stream
                        .send(tungstenite::Message::Text(
                            serde_json::to_string(&SocketModeAcknowledgeMessage {
                                envelope_id,
                                payload: None,
                            })
                            .expect("Failed to serialize ack message"),
                        ))
                        .await
                        .expect("Failed to reply ack message");

                    match serde_json::from_str::<serde_json::Value>(&t) {
                        Ok(v) => {
                            let event = v
                                .get("payload")
                                .and_then(|v| v.get("event"))
                                .expect("Failed to get event");
                            slack_client
                                .send_message(
                                    event
                                        .get("channel")
                                        .and_then(|v| v.as_str())
                                        .expect("Failed to get channel id"),
                                    &format!(
                                        "You said: {}",
                                        format!(
                                            "```{}```",
                                            event
                                                .get("text")
                                                .and_then(|v| v.as_str())
                                                .expect("Failed to get text")
                                        )
                                    ),
                                )
                                .await
                                .expect("Failed to send message");
                        }
                        Err(e) => {
                            println!("Failed to parse event: {}", e);
                        }
                    }
                }
                Err(e) => {
                    println!("Unknown text frame: {}: {:?}", t, e);
                }
            },
            tungstenite::Message::Ping(bytes) => {
                println!("ping: {:?}", bytes);
            }
            _ => println!("Unknown frame"),
        }
    }
}
