
use anyhow::{Context, Result};
use futures_util::StreamExt;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::ruma::events::relation::InReplyTo;
use matrix_sdk::ruma::events::room::{
    message::{MessageType, OriginalRoomMessageEvent, OriginalSyncRoomMessageEvent, Relation, RoomMessageEventContent},
    MediaSource,
};
use matrix_sdk::ruma::events::receipt::{ReceiptEventContent, ReceiptType};
use matrix_sdk::ruma::events::SyncEphemeralRoomEvent;
use matrix_sdk::ruma::{uint, RoomId};
use matrix_sdk::encryption::verification::{
    AcceptSettings, SasState, SasVerification, VerificationRequestState,
};
use matrix_sdk::encryption::EncryptionSettings;
use matrix_sdk::matrix_auth::MatrixSession;
use matrix_sdk::attachment::AttachmentConfig;
use matrix_sdk::room::{MessagesOptions, Room};
use matrix_sdk::media::{MediaEventContent, MediaFormat, MediaRequest};
use matrix_sdk::{Client, RoomState};
use matrix_sdk::DisplayName;
use matrix_sdk::ruma::events::key::verification::{ShortAuthenticationString, VerificationMethod};
use mime_guess::from_path;
use tokio::sync::{mpsc, Mutex};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::fs;

use crate::config::AccountConfig;
use crate::storage::{append_message, latest_room_timestamp, StoredMessage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomListState {
    Joined,
    Invited,
}

#[derive(Debug, Clone)]
pub struct RoomInfo {
    pub room_id: String,
    pub name: String,
    pub state: RoomListState,
    pub inviter: Option<String>,
}

#[derive(Debug)]
pub enum MatrixEvent {
    Rooms(Vec<RoomInfo>),
    Message {
        room_id: String,
        event_id: String,
        sender: String,
        body: String,
        timestamp: i64,
        reply_to: Option<String>,
    },
    Attachment {
        room_id: String,
        event_id: String,
        sender: String,
        name: String,
        path: String,
        kind: String,
        timestamp: i64,
        reply_to: Option<String>,
    },
    Receipt {
        room_id: String,
        event_id: String,
    },
    BackfillDone,
    VerificationStatus {
        message: String,
    },
    VerificationEmojis {
        emojis: Vec<(String, String)>,
    },
    VerificationDone,
    VerificationCancelled {
        reason: String,
    },
}

#[derive(Debug)]
pub enum MatrixCommand {
    SendMessage {
        room_id: String,
        body: String,
        reply_to: Option<String>,
    },
    SendAttachment {
        room_id: String,
        path: String,
        reply_to: Option<String>,
    },
    JoinRoom { room: String },
    CreateDirect { user_id: String },
    LeaveRoom { room_id: String },
    AcceptInvite { room_id: String },
    RejectInvite { room_id: String },
    StartVerification,
    ConfirmVerification,
    CancelVerification,
}

pub async fn build_client(homeserver: &str, passphrase: &str) -> Result<Client> {
    let crypto_dir = crate::config::crypto_dir().context("crypto dir")?;
    let settings = EncryptionSettings {
        auto_enable_cross_signing: true,
        auto_enable_backups: true,
        ..Default::default()
    };
    Client::builder()
        .homeserver_url(homeserver)
        .sqlite_store(crypto_dir, Some(passphrase))
        .with_encryption_settings(settings)
        .build()
        .await
        .context("create matrix client")
}

pub async fn login_with_client(
    client: &Client,
    homeserver: &str,
    username: &str,
    password: &str,
) -> Result<AccountConfig> {
    let response = client
        .matrix_auth()
        .login_username(username, password)
        .initial_device_display_name("marty")
        .send()
        .await
        .context("matrix login")?;

    let session = MatrixSession::from(&response);
    Ok(AccountConfig {
        homeserver: homeserver.to_string(),
        username: username.to_string(),
        user_id: Some(response.user_id.to_string()),
        display_name: None,
        session_encrypted: None,
        session: Some(session),
    })
}

pub async fn start_sync(
    client: Client,
    passphrase: String,
    mut cmd_rx: mpsc::UnboundedReceiver<MatrixCommand>,
    evt_tx: mpsc::UnboundedSender<MatrixEvent>,
) -> Result<()> {
    let sas_state: Arc<Mutex<Option<SasVerification>>> = Arc::new(Mutex::new(None));
    let _ = client.sync_once(SyncSettings::default()).await;
    publish_rooms(&client, &evt_tx).await;
    backfill_since_last_seen(&client, &passphrase, &evt_tx).await;
    let _ = evt_tx.send(MatrixEvent::BackfillDone);

    let evt_tx_clone = evt_tx.clone();
    let passphrase_clone = passphrase.clone();
    let own_user = client.user_id().map(|id| id.to_owned());
    client
        .add_event_handler(move |ev: OriginalSyncRoomMessageEvent, room: Room| {
            let evt_tx = evt_tx_clone.clone();
            let passphrase = passphrase_clone.clone();
            async move {
                if room.state() != RoomState::Joined {
                    return;
                }
                let room_id = room.room_id().to_string();
                let event_id = ev.event_id.to_string();
                let sender = ev.sender.to_string();
                let ts = i64::from(ev.origin_server_ts.0);
                let reply_to = extract_reply_to(&ev.content);
                match &ev.content.msgtype {
                    MessageType::Text(text) => {
                        let body = text.body.clone();
                        let _ = evt_tx.send(MatrixEvent::Message {
                            room_id: room_id.clone(),
                            event_id: event_id.clone(),
                            sender: sender.clone(),
                            body: body.clone(),
                            timestamp: ts,
                            reply_to: reply_to.clone(),
                        });
                        let _ = store_message_encrypted(
                            &passphrase,
                            &room_id,
                            ts,
                            &sender,
                            &body,
                            Some(&event_id),
                            reply_to.as_deref(),
                            None,
                        );
                    }
                    MessageType::Image(content) => {
                        handle_attachment_event(
                            &room,
                            &passphrase,
                            &evt_tx,
                            &room_id,
                            &event_id,
                            &sender,
                            ts,
                            "image",
                            &content.body,
                            reply_to.clone(),
                            content,
                        )
                        .await;
                    }
                    MessageType::File(content) => {
                        handle_attachment_event(
                            &room,
                            &passphrase,
                            &evt_tx,
                            &room_id,
                            &event_id,
                            &sender,
                            ts,
                            "file",
                            &content.body,
                            reply_to.clone(),
                            content,
                        )
                        .await;
                    }
                    MessageType::Video(content) => {
                        handle_attachment_event(
                            &room,
                            &passphrase,
                            &evt_tx,
                            &room_id,
                            &event_id,
                            &sender,
                            ts,
                            "video",
                            &content.body,
                            reply_to.clone(),
                            content,
                        )
                        .await;
                    }
                    MessageType::Audio(content) => {
                        handle_attachment_event(
                            &room,
                            &passphrase,
                            &evt_tx,
                            &room_id,
                            &event_id,
                            &sender,
                            ts,
                            "audio",
                            &content.body,
                            reply_to.clone(),
                            content,
                        )
                        .await;
                    }
                    _ => {}
                }
            }
        });

    let evt_tx_receipts = evt_tx.clone();
    let own_user_receipts = own_user.clone();
    client.add_event_handler(move |ev: SyncEphemeralRoomEvent<ReceiptEventContent>, room: Room| {
        let evt_tx = evt_tx_receipts.clone();
        let own_user = own_user_receipts.clone();
        async move {
            if room.state() != RoomState::Joined {
                return;
            }
            let room_id = room.room_id().to_string();
            let content = ev.content;
            for (event_id, receipts) in content.0 {
                let Some(users) = receipts.get(&ReceiptType::Read) else {
                    continue;
                };
                for (user_id, _) in users {
                    if own_user
                        .as_ref()
                        .is_some_and(|u| u.as_str() == user_id.as_str())
                    {
                        continue;
                    }
                    let _ = evt_tx.send(MatrixEvent::Receipt {
                        room_id: room_id.clone(),
                        event_id: event_id.to_string(),
                    });
                    break;
                }
            }
        }
    });

    let sync_client = client.clone();
    let sync_task = tokio::spawn(async move {
        let _ = sync_client.sync(SyncSettings::default()).await;
    });

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            MatrixCommand::SendMessage {
                room_id,
                body,
                reply_to,
            } => {
                if let Ok(room_id) = RoomId::parse(&room_id) {
                    if let Some(room) = client.get_room(&room_id) {
                        let mut content = RoomMessageEventContent::text_plain(body.clone());
                        if let Some(reply_to) = reply_to {
                            if let Ok(event_id) = reply_to.parse() {
                                content.relates_to = Some(Relation::Reply {
                                    in_reply_to: InReplyTo::new(event_id),
                                });
                            }
                        }
                        let _ = room.send(content).await;
                    }
                }
            }
            MatrixCommand::SendAttachment {
                room_id,
                path,
                reply_to,
            } => {
                let _reply_to = reply_to;
                if let Ok(room_id) = RoomId::parse(&room_id) {
                    if let Some(room) = client.get_room(&room_id) {
                        let Ok(data) = fs::read(&path) else {
                            continue;
                        };
                        let body = Path::new(&path)
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("attachment");
                        let mime = from_path(&path).first_or_octet_stream();
                        let _ = room
                            .send_attachment(body, &mime, data, AttachmentConfig::new())
                            .await;
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
            MatrixCommand::LeaveRoom { room_id } => {
                if let Ok(room_id) = RoomId::parse(&room_id) {
                    if let Some(room) = client.get_room(&room_id) {
                        let _ = room.leave().await;
                        publish_rooms(&client, &evt_tx).await;
                    }
                }
            }
            MatrixCommand::AcceptInvite { room_id } => {
                if let Ok(room_id) = RoomId::parse(&room_id) {
                    if let Some(room) = client.get_room(&room_id) {
                        let _ = room.join().await;
                        publish_rooms(&client, &evt_tx).await;
                    }
                }
            }
            MatrixCommand::RejectInvite { room_id } => {
                if let Ok(room_id) = RoomId::parse(&room_id) {
                    if let Some(room) = client.get_room(&room_id) {
                        let _ = room.leave().await;
                        publish_rooms(&client, &evt_tx).await;
                    }
                }
            }
            MatrixCommand::StartVerification => {
                let Some(user_id) = client.user_id() else { continue };
                if let Ok(Some(user)) = client.encryption().get_user_identity(user_id).await {
                    if let Ok(request) = user
                        .request_verification_with_methods(vec![VerificationMethod::SasV1])
                        .await
                    {
                        let evt_tx = evt_tx.clone();
                        let sas_state = sas_state.clone();
                        let _ = evt_tx.send(MatrixEvent::VerificationStatus {
                            message: "Waiting for other device...".to_string(),
                        });
                        tokio::spawn(async move {
                            let mut changes = request.changes();
                            let mut started = false;
                            while let Some(state) = changes.next().await {
                                match state {
                                    VerificationRequestState::Transitioned { verification } => {
                                        if let Some(sas) = verification.sas() {
                                            started = true;
                                            let _ = evt_tx.send(MatrixEvent::VerificationStatus {
                                                message: "SAS started. Waiting for emojis...".to_string(),
                                            });
                                            start_sas_flow(sas, &sas_state, &evt_tx).await;
                                        }
                                    }
                                    VerificationRequestState::Ready { .. } => {
                                        if started {
                                            continue;
                                        }
                                        let _ = evt_tx.send(MatrixEvent::VerificationStatus {
                                            message: "SAS requested. Waiting for emojis...".to_string(),
                                        });
                                        if let Ok(Some(sas)) = request.start_sas().await {
                                            started = true;
                                            start_sas_flow(sas, &sas_state, &evt_tx).await;
                                        }
                                    }
                                    VerificationRequestState::Cancelled(cancel) => {
                                        let _ = evt_tx.send(MatrixEvent::VerificationCancelled {
                                            reason: cancel.reason().to_string(),
                                        });
                                        break;
                                    }
                                    VerificationRequestState::Done => {
                                        let _ = evt_tx.send(MatrixEvent::VerificationDone);
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        });
                    }
                }
            }
            MatrixCommand::ConfirmVerification => {
                if let Some(sas) = sas_state.lock().await.take() {
                    let _ = sas.confirm().await;
                }
            }
            MatrixCommand::CancelVerification => {
                if let Some(sas) = sas_state.lock().await.take() {
                    let _ = sas.mismatch().await;
                }
            }
        }
    }

    let _ = sync_task.await;
    Ok(())
}

async fn publish_rooms(client: &Client, evt_tx: &mpsc::UnboundedSender<MatrixEvent>) {
    let joined_rooms = client.joined_rooms();
    let invited_rooms = client.invited_rooms();
    let mut room_infos = Vec::new();
    for room in joined_rooms {
        let room_id = room.room_id().to_string();
        let name = match room.display_name().await {
            Ok(DisplayName::Empty) | Ok(DisplayName::EmptyWas(_)) => {
                resolve_room_name(client, &room, &room_id).await
            }
            Ok(name) => name.to_string(),
            Err(_) => resolve_room_name(client, &room, &room_id).await,
        };
        room_infos.push(RoomInfo {
            room_id,
            name,
            state: RoomListState::Joined,
            inviter: None,
        });
    }
    for room in invited_rooms {
        let room_id = room.room_id().to_string();
        let inviter = room
            .invite_details()
            .await
            .ok()
            .and_then(|invite| invite.inviter)
            .map(|inviter| inviter.name().to_string())
            .filter(|name| !name.is_empty());
        let name = match room.display_name().await {
            Ok(DisplayName::Empty) | Ok(DisplayName::EmptyWas(_)) => {
                resolve_room_name(client, &room, &room_id).await
            }
            Ok(name) => name.to_string(),
            Err(_) => resolve_room_name(client, &room, &room_id).await,
        };
        let name = if (name == room_id || name == "Empty Room") && inviter.is_some() {
            inviter.clone().unwrap_or(name)
        } else {
            name
        };
        room_infos.push(RoomInfo {
            room_id,
            name,
            state: RoomListState::Invited,
            inviter,
        });
    }
    let _ = evt_tx.send(MatrixEvent::Rooms(room_infos));
}

enum BackfillItem {
    Text {
        event_id: String,
        sender: String,
        body: String,
        timestamp: i64,
        reply_to: Option<String>,
    },
    Attachment {
        event_id: String,
        sender: String,
        name: String,
        path: String,
        kind: String,
        timestamp: i64,
        reply_to: Option<String>,
    },
}

struct AttachmentInfo {
    kind: String,
    name: String,
    path: String,
}

async fn backfill_since_last_seen(
    client: &Client,
    passphrase: &str,
    evt_tx: &mpsc::UnboundedSender<MatrixEvent>,
) {
    let Ok(messages_dir) = crate::config::messages_dir() else {
        return;
    };
    for room in client.joined_rooms() {
        let room_id = room.room_id().to_string();
        let last_ts = match latest_room_timestamp(&messages_dir, &room_id, passphrase) {
            Ok(Some(ts)) => ts,
            _ => continue,
        };
        let mut from: Option<String> = None;
        let mut collected: Vec<BackfillItem> = Vec::new();
        loop {
            let mut options = MessagesOptions::backward();
            options.limit = uint!(50);
            if let Some(token) = from.as_ref() {
                options.from = Some(token.clone());
            }
            let Ok(messages) = room.messages(options).await else {
                break;
            };
            if messages.chunk.is_empty() {
                break;
            }
            let mut stop = false;
            for event in messages.chunk {
                let Ok(message) = event.event.deserialize_as::<OriginalRoomMessageEvent>() else {
                    continue;
                };
                let ts = i64::from(message.origin_server_ts.0);
                if ts <= last_ts {
                    stop = true;
                    break;
                }
                match &message.content.msgtype {
                    MessageType::Text(text) => {
                        collected.push(BackfillItem::Text {
                            event_id: message.event_id.to_string(),
                            sender: message.sender.to_string(),
                            body: text.body.clone(),
                            timestamp: ts,
                            reply_to: extract_reply_to(&message.content),
                        });
                    }
                    MessageType::Image(content) => {
                        if let Some(item) = backfill_attachment(
                            &room,
                            &message.event_id.to_string(),
                            &message.sender.to_string(),
                            ts,
                            "image",
                            &content.body,
                            extract_reply_to(&message.content),
                            content,
                        )
                        .await
                        {
                            collected.push(item);
                        }
                    }
                    MessageType::File(content) => {
                        if let Some(item) = backfill_attachment(
                            &room,
                            &message.event_id.to_string(),
                            &message.sender.to_string(),
                            ts,
                            "file",
                            &content.body,
                            extract_reply_to(&message.content),
                            content,
                        )
                        .await
                        {
                            collected.push(item);
                        }
                    }
                    MessageType::Video(content) => {
                        if let Some(item) = backfill_attachment(
                            &room,
                            &message.event_id.to_string(),
                            &message.sender.to_string(),
                            ts,
                            "video",
                            &content.body,
                            extract_reply_to(&message.content),
                            content,
                        )
                        .await
                        {
                            collected.push(item);
                        }
                    }
                    MessageType::Audio(content) => {
                        if let Some(item) = backfill_attachment(
                            &room,
                            &message.event_id.to_string(),
                            &message.sender.to_string(),
                            ts,
                            "audio",
                            &content.body,
                            extract_reply_to(&message.content),
                            content,
                        )
                        .await
                        {
                            collected.push(item);
                        }
                    }
                    _ => {}
                }
            }
            if stop {
                break;
            }
            match messages.end {
                Some(token) => from = Some(token),
                None => break,
            }
        }
        collected.sort_by_key(|msg| match msg {
            BackfillItem::Text { timestamp, .. } => *timestamp,
            BackfillItem::Attachment { timestamp, .. } => *timestamp,
        });
        for msg in collected {
            match msg {
                BackfillItem::Text {
                    event_id,
                    sender,
                    body,
                    timestamp,
                    reply_to,
                } => {
                    let _ = evt_tx.send(MatrixEvent::Message {
                        room_id: room_id.clone(),
                        event_id: event_id.clone(),
                        sender: sender.clone(),
                        body: body.clone(),
                        timestamp,
                        reply_to: reply_to.clone(),
                    });
                    let _ = store_message_encrypted(
                        passphrase,
                        &room_id,
                        timestamp,
                        &sender,
                        &body,
                        Some(&event_id),
                        reply_to.as_deref(),
                        None,
                    );
                }
                BackfillItem::Attachment {
                    event_id,
                    sender,
                    name,
                    path,
                    kind,
                    timestamp,
                    reply_to,
                } => {
                    let name_for_store = name.clone();
                    let name_for_attachment = name.clone();
                    let path_clone = path.clone();
                    let _ = evt_tx.send(MatrixEvent::Attachment {
                        room_id: room_id.clone(),
                        event_id: event_id.clone(),
                        sender: sender.clone(),
                        name: name.clone(),
                        path: path.clone(),
                        kind: kind.clone(),
                        timestamp,
                        reply_to: reply_to.clone(),
                    });
                    let _ = store_message_encrypted(
                        passphrase,
                        &room_id,
                        timestamp,
                        &sender,
                        &name_for_store,
                        Some(&event_id),
                        reply_to.as_deref(),
                        Some(AttachmentInfo {
                            kind,
                            name: name_for_attachment,
                            path: path_clone,
                        }),
                    );
                }
            }
        }
    }
}

async fn handle_attachment_event<T: MediaEventContent + ?Sized>(
    room: &Room,
    passphrase: &str,
    evt_tx: &mpsc::UnboundedSender<MatrixEvent>,
    room_id: &str,
    event_id: &str,
    sender: &str,
    ts: i64,
    kind: &str,
    body: &str,
    reply_to: Option<String>,
    content: &T,
) {
    let Some(source) = content.source() else {
        return;
    };
    let name = attachment_name(body, kind);
    match download_attachment(room, &source, &name).await {
        Ok(path) => {
            let path_str = path.to_string_lossy().to_string();
            let _ = evt_tx.send(MatrixEvent::Attachment {
                room_id: room_id.to_string(),
                event_id: event_id.to_string(),
                sender: sender.to_string(),
                name: name.clone(),
                path: path_str.clone(),
                kind: kind.to_string(),
                timestamp: ts,
                reply_to: reply_to.clone(),
            });
            let _ = store_message_encrypted(
                passphrase,
                room_id,
                ts,
                sender,
                &name,
                Some(event_id),
                reply_to.as_deref(),
                Some(AttachmentInfo {
                    kind: kind.to_string(),
                    name: name.clone(),
                    path: path_str.clone(),
                }),
            );
        }
        Err(_) => {
            let fallback = format!("[{}] {}", kind, name);
            let _ = evt_tx.send(MatrixEvent::Message {
                room_id: room_id.to_string(),
                event_id: event_id.to_string(),
                sender: sender.to_string(),
                body: fallback.clone(),
                timestamp: ts,
                reply_to: reply_to.clone(),
            });
            let _ = store_message_encrypted(
                passphrase,
                room_id,
                ts,
                sender,
                &fallback,
                Some(event_id),
                reply_to.as_deref(),
                None,
            );
        }
    }
}

async fn backfill_attachment<T: MediaEventContent + ?Sized>(
    room: &Room,
    event_id: &str,
    sender: &str,
    ts: i64,
    kind: &str,
    body: &str,
    reply_to: Option<String>,
    content: &T,
) -> Option<BackfillItem> {
    let Some(source) = content.source() else {
        return None;
    };
    let name = attachment_name(body, kind);
    match download_attachment(room, &source, &name).await {
        Ok(path) => Some(BackfillItem::Attachment {
            event_id: event_id.to_string(),
            sender: sender.to_string(),
            name,
            path: path.to_string_lossy().to_string(),
            kind: kind.to_string(),
            timestamp: ts,
            reply_to,
        }),
        Err(_) => Some(BackfillItem::Text {
            event_id: event_id.to_string(),
            sender: sender.to_string(),
            body: format!("[{}] {}", kind, name),
            timestamp: ts,
            reply_to,
        }),
    }
}

async fn download_attachment(room: &Room, source: &MediaSource, name: &str) -> Result<PathBuf> {
    let request = MediaRequest {
        source: source.clone(),
        format: MediaFormat::File,
    };
    let data = room.client().media().get_media_content(&request, true).await?;
    let dir = crate::config::attachments_dir()?;
    fs::create_dir_all(&dir)?;
    let filename = sanitize_filename(name);
    let path = unique_path(&dir, &filename);
    fs::write(&path, data)?;
    Ok(path)
}

fn attachment_name(body: &str, fallback: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

fn extract_reply_to(content: &RoomMessageEventContent) -> Option<String> {
    match content.relates_to.as_ref() {
        Some(Relation::Reply { in_reply_to }) => Some(in_reply_to.event_id.to_string()),
        _ => None,
    }
}

fn sanitize_filename(name: &str) -> String {
    let cleaned = name.replace(['/', '\\'], "_");
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "attachment".to_string()
    } else {
        trimmed.to_string()
    }
}

fn unique_path(dir: &Path, filename: &str) -> PathBuf {
    let mut path = dir.join(filename);
    if !path.exists() {
        return path;
    }
    let (stem, ext) = match path.file_name().and_then(|name| name.to_str()) {
        Some(name) => match name.rsplit_once('.') {
            Some((s, e)) => (s.to_string(), Some(e.to_string())),
            None => (name.to_string(), None),
        },
        None => ("attachment".to_string(), None),
    };
    for idx in 1..1000 {
        let candidate = match &ext {
            Some(ext) => format!("{}-{}.{}", stem, idx, ext),
            None => format!("{}-{}", stem, idx),
        };
        path = dir.join(candidate);
        if !path.exists() {
            return path;
        }
    }
    dir.join(format!("{}-{}", stem, uuid_suffix()))
}

fn uuid_suffix() -> String {
    format!("{:x}", rand::random::<u64>())
}

async fn resolve_room_name(client: &Client, room: &Room, fallback: &str) -> String {
    let own_id = client.user_id().map(|id| id.as_str());
    if let Some(target) = room
        .direct_targets()
        .into_iter()
        .find(|user| Some(user.as_str()) != own_id)
    {
        return format_user_id(target.as_str());
    }
    if let Some(name) = room.name() {
        return name;
    }
    if let Some(alias) = room.canonical_alias().or_else(|| room.alt_aliases().pop()) {
        return alias.to_string();
    }
    fallback.to_string()
}

fn format_user_id(user_id: &str) -> String {
    user_id
        .trim_start_matches('@')
        .split(':')
        .next()
        .unwrap_or(user_id)
        .to_string()
}

async fn start_sas_flow(
    sas: SasVerification,
    sas_state: &Arc<Mutex<Option<SasVerification>>>,
    evt_tx: &mpsc::UnboundedSender<MatrixEvent>,
) {
    let settings = AcceptSettings::with_allowed_methods(vec![ShortAuthenticationString::Emoji]);
    let _ = sas.accept_with_settings(settings).await;
    {
        let mut guard = sas_state.lock().await;
        *guard = Some(sas.clone());
    }
    let evt_tx = evt_tx.clone();
    tokio::spawn(async move {
        let mut sas_changes = sas.changes();
        while let Some(state) = sas_changes.next().await {
            match state {
                SasState::KeysExchanged { emojis, .. } => {
                    if let Some(emojis) = emojis {
                        let pairs = emojis
                            .emojis
                            .iter()
                            .map(|e| (e.symbol.to_string(), e.description.to_string()))
                            .collect();
                        let _ = evt_tx.send(MatrixEvent::VerificationEmojis { emojis: pairs });
                    }
                }
                SasState::Done { .. } => {
                    let _ = evt_tx.send(MatrixEvent::VerificationDone);
                    break;
                }
                SasState::Cancelled(cancel) => {
                    let _ = evt_tx.send(MatrixEvent::VerificationCancelled {
                        reason: cancel.reason().to_string(),
                    });
                    break;
                }
                _ => {}
            }
        }
    });
}


fn store_message_encrypted(
    passphrase: &str,
    room_id: &str,
    ts: i64,
    sender: &str,
    body: &str,
    event_id: Option<&str>,
    reply_to: Option<&str>,
    attachment: Option<AttachmentInfo>,
) -> Result<()> {
    let messages_dir = crate::config::messages_dir()?;
    let record = StoredMessage {
        timestamp: ts,
        sender: sender.to_string(),
        body: body.to_string(),
        event_id: event_id.map(|id| id.to_string()),
        reply_to: reply_to.map(|id| id.to_string()),
        attachment_path: attachment.as_ref().map(|info| info.path.clone()),
        attachment_name: attachment.as_ref().map(|info| info.name.clone()),
        attachment_kind: attachment.map(|info| info.kind),
    };
    append_message(&messages_dir, passphrase, room_id, record)?;
    Ok(())
}
