use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use matrix_sdk::config::SyncSettings;
use matrix_sdk::ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent};
use matrix_sdk::ruma::RoomId;
use matrix_sdk::matrix_auth::MatrixSession;
use matrix_sdk::room::MessagesOptions;
use matrix_sdk::{room::Room, Client, RoomState};
use matrix_sdk::ruma::events::{AnyMessageLikeEvent, AnyTimelineEvent, MessageLikeEvent};
use matrix_sdk::ruma::events::room::message::OriginalRoomMessageEvent;
use matrix_sdk::ruma::uint;
use tokio::sync::mpsc;

use crate::config::AccountConfig;

#[derive(Debug, Clone)]
pub struct RoomInfo {
    pub room_id: String,
    pub name: String,
}

#[derive(Debug)]
pub enum MatrixEvent {
    Rooms(Vec<RoomInfo>),
    Message {
        room_id: String,
        sender: String,
        body: String,
        timestamp: i64,
    },
}

#[derive(Debug)]
pub enum MatrixCommand {
    SendMessage { room_id: String, body: String },
    JoinRoom { room: String },
    CreateDirect { user_id: String },
}

pub async fn build_client(homeserver: &str) -> Result<Client> {
    Client::builder()
        .homeserver_url(homeserver)
        .build()
        .await
        .context("create matrix client")
}

pub async fn login(
    homeserver: &str,
    username: &str,
    password: &str,
) -> Result<(Client, AccountConfig)> {
    let client = build_client(homeserver).await?;
    let response = client
        .matrix_auth()
        .login_username(username, password)
        .initial_device_display_name("marty")
        .send()
        .await
        .context("matrix login")?;

    let session = MatrixSession::from(&response);
    let account = AccountConfig {
        homeserver: homeserver.to_string(),
        username: username.to_string(),
        user_id: Some(response.user_id.to_string()),
        display_name: None,
        session: Some(session),
    };

    Ok((client, account))
}

pub async fn start_sync(
    client: Client,
    data_dir: PathBuf,
    mut cmd_rx: mpsc::UnboundedReceiver<MatrixCommand>,
    evt_tx: mpsc::UnboundedSender<MatrixEvent>,
) -> Result<()> {
    let _ = client.sync_once(SyncSettings::default()).await;
    publish_rooms(&client, &evt_tx).await;
    backfill_rooms(&client, &data_dir, &evt_tx).await;

    let evt_tx_clone = evt_tx.clone();
    let data_dir_clone = data_dir.clone();
    client
        .add_event_handler(move |ev: OriginalSyncRoomMessageEvent, room: Room| {
            let evt_tx = evt_tx_clone.clone();
            let data_dir = data_dir_clone.clone();
            async move {
                if room.state() != RoomState::Joined {
                    return;
                }
                let MessageType::Text(text) = &ev.content.msgtype else { return; };
                let room_id = room.room_id().to_string();
                let sender = ev.sender.to_string();
                let body = text.body.clone();
                let ts = i64::from(ev.origin_server_ts.0);
                let _ = evt_tx
                    .send(MatrixEvent::Message {
                        room_id: room_id.clone(),
                        sender: sender.clone(),
                        body: body.clone(),
                        timestamp: ts,
                    });
                let _ = store_message(&data_dir, &room_id, ts, &sender, &body);
            }
        });

    let sync_client = client.clone();
    let sync_task = tokio::spawn(async move {
        let _ = sync_client.sync(SyncSettings::default()).await;
    });

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            MatrixCommand::SendMessage { room_id, body } => {
                if let Ok(room_id) = RoomId::parse(&room_id) {
                    if let Some(room) = client.get_room(&room_id) {
                        let content = matrix_sdk::ruma::events::room::message::RoomMessageEventContent::text_plain(
                            body,
                        );
                        let _ = room.send(content).await;
                    }
                }
            }
            MatrixCommand::JoinRoom { room } => {
                if let Ok(room_or_alias) = matrix_sdk::ruma::RoomOrAliasId::parse(&room) {
                    let _ = client.join_room_by_id_or_alias(&room_or_alias, &[]).await;
                    publish_rooms(&client, &evt_tx).await;
                }
            }
            MatrixCommand::CreateDirect { user_id } => {
                if let Ok(user_id) = matrix_sdk::ruma::UserId::parse(&user_id) {
                    let mut request =
                        matrix_sdk::ruma::api::client::room::create_room::v3::Request::new();
                    request.is_direct = true;
                    request.invite.push(user_id.to_owned());
                    let _ = client.create_room(request).await;
                    publish_rooms(&client, &evt_tx).await;
                }
            }
        }
    }

    let _ = sync_task.await;
    Ok(())
}

async fn publish_rooms(client: &Client, evt_tx: &mpsc::UnboundedSender<MatrixEvent>) {
    let rooms = client.joined_rooms();
    let mut room_infos = Vec::new();
    for room in rooms {
        let room_id = room.room_id().to_string();
        let name = room
            .display_name()
            .await
            .map(|name| name.to_string())
            .unwrap_or_else(|_| room_id.clone());
        room_infos.push(RoomInfo { room_id, name });
    }
    let _ = evt_tx.send(MatrixEvent::Rooms(room_infos));
}

async fn backfill_rooms(
    client: &Client,
    data_dir: &Path,
    evt_tx: &mpsc::UnboundedSender<MatrixEvent>,
) {
    for room in client.joined_rooms() {
        let room_id = room.room_id().to_string();
        let mut options = MessagesOptions::backward();
        options.limit = uint!(50);
        if let Ok(messages) = room.messages(options).await {
            for event in messages.chunk.into_iter().rev() {
                if let Some((sender, body, ts)) = extract_text_message(&event.event) {
                    let _ = evt_tx.send(MatrixEvent::Message {
                        room_id: room_id.clone(),
                        sender: sender.clone(),
                        body: body.clone(),
                        timestamp: ts,
                    });
                    let _ = store_message(data_dir, &room_id, ts, &sender, &body);
                }
            }
        }
    }
}

fn extract_text_message(raw: &matrix_sdk::ruma::serde::Raw<AnyTimelineEvent>) -> Option<(String, String, i64)> {
    let event = raw.deserialize().ok()?;
    if let AnyTimelineEvent::MessageLike(AnyMessageLikeEvent::RoomMessage(MessageLikeEvent::Original(
        OriginalRoomMessageEvent { sender, content, origin_server_ts, .. },
    ))) = event
    {
        if let MessageType::Text(text) = content.msgtype {
            let ts = i64::from(origin_server_ts.0);
            return Some((sender.to_string(), text.body, ts));
        }
    }
    None
}

fn store_message(
    data_dir: &Path,
    room_id: &str,
    ts: i64,
    sender: &str,
    body: &str,
) -> Result<()> {
    let room_dir = data_dir.join("messages").join(room_id.replace(':', "_"));
    fs::create_dir_all(&room_dir)?;
    let path = room_dir.join("messages.jsonl");
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let record = serde_json::json!({
        "timestamp": ts,
        "sender": sender,
        "body": body,
    });
    writeln!(file, "{}", record)?;
    Ok(())
}
