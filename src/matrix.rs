
use anyhow::{Context, Result};
use futures_util::StreamExt;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent};
use matrix_sdk::ruma::RoomId;
use matrix_sdk::encryption::verification::{
    AcceptSettings, SasState, SasVerification, VerificationRequestState,
};
use matrix_sdk::encryption::EncryptionSettings;
use matrix_sdk::matrix_auth::MatrixSession;
use matrix_sdk::{room::Room, Client, RoomState};
use matrix_sdk::ruma::events::key::verification::{ShortAuthenticationString, VerificationMethod};
use tokio::sync::{mpsc, Mutex};
use std::sync::Arc;

use crate::config::AccountConfig;
use crate::storage::{append_message, StoredMessage};

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
        event_id: String,
        sender: String,
        body: String,
        timestamp: i64,
    },
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
    SendMessage { room_id: String, body: String },
    JoinRoom { room: String },
    CreateDirect { user_id: String },
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

    let evt_tx_clone = evt_tx.clone();
    let passphrase_clone = passphrase.clone();
    client
        .add_event_handler(move |ev: OriginalSyncRoomMessageEvent, room: Room| {
            let evt_tx = evt_tx_clone.clone();
            let passphrase = passphrase_clone.clone();
            async move {
                if room.state() != RoomState::Joined {
                    return;
                }
                let MessageType::Text(text) = &ev.content.msgtype else { return; };
                let room_id = room.room_id().to_string();
                let event_id = ev.event_id.to_string();
                let sender = ev.sender.to_string();
                let body = text.body.clone();
                let ts = i64::from(ev.origin_server_ts.0);
                let _ = evt_tx
                    .send(MatrixEvent::Message {
                        room_id: room_id.clone(),
                        event_id: event_id.clone(),
                        sender: sender.clone(),
                        body: body.clone(),
                        timestamp: ts,
                    });
                let _ = store_message_encrypted(
                    &passphrase,
                    &room_id,
                    ts,
                    &sender,
                    &body,
                    Some(&event_id),
                );
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
                            body.clone(),
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
) -> Result<()> {
    let messages_dir = crate::config::messages_dir()?;
    let record = StoredMessage {
        timestamp: ts,
        sender: sender.to_string(),
        body: body.to_string(),
        event_id: event_id.map(|id| id.to_string()),
    };
    append_message(&messages_dir, passphrase, room_id, record)?;
    Ok(())
}
