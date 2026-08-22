use std::{sync::Arc, time::Duration};

use better_tokio_select::tokio_select;
use deform_core::{DeformError, DeformUserLogic, Pubkey, error::UserFacingResult};
use quinn::Connection;
use tokio::{
    sync::{broadcast, mpsc},
    time::sleep,
};
use tracing::{info, warn};

use crate::{
    Compressed, DeformQuicLogic, ReliableMessage, ServerInstruction, StateUpdatePacket,
    datagram::{DatagramDefragmentor, DatagramFragmentor},
    server::{
        DeformQuicServer,
        matches::{InternalServerBroadcast, MatchMessage},
    },
};

impl<Q: DeformQuicLogic> DeformQuicServer<Q> {
    // NOTE: Errors from client_loop get propagated backward, closing the connection and sending an error message to the client
    pub async fn client_loop(
        pubkey: Pubkey,
        match_sender: mpsc::Sender<MatchMessage<Q::UserLogic>>,
        connection: Connection,
        control_send: &mut quinn::SendStream,
        mut state_receiver: broadcast::Receiver<Arc<InternalServerBroadcast<Q>>>,
    ) -> UserFacingResult<Q::UserLogic> {
        match_sender
            .send(MatchMessage::PlayerJoined { pubkey })
            .await
            .map_err(|_| DeformError::ChannelClosed)?;

        // Each holds its own `Connection` handle, which is just an Arc, so `connection`
        // stays usable below for closing.
        let mut fragmentor = DatagramFragmentor::new(connection.clone());
        let mut defragmentor = DatagramDefragmentor::<Q>::new(connection.clone());

        loop {
            tokio_select!(match .. {
                .. if let internal_broadcast = state_receiver.recv() => {
                    match internal_broadcast {
                        Ok(internal_broadcast) => {
                            let finished = Self::forward_to_client(
                                pubkey,
                                &connection,
                                control_send,
                                &internal_broadcast,
                                &mut fragmentor,
                            )
                            .await?;

                            if finished {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            Err(DeformError::ChannelClosed)?;
                        }
                        // This client task fell behind the match task and the broadcast
                        // dropped snapshots for it. Silently continuing looks exactly
                        // like the network losing them, so say so.
                        Err(broadcast::error::RecvError::Lagged(missed)) => {
                            warn!(player = %pubkey, missed, "client task lagged; snapshots dropped");
                            continue;
                        }
                    }
                }
                // instead of triggering every time a datagram is received, this waits for an entire message to be collected first
                .. if let message = defragmentor.recv() => {
                    // A client is not trusted, so one datagram it malformed must not end its match
                    let message = match message {
                        Ok(message) => message,
                        Err(e @ DeformError::Connection(_)) => Err(e)?,
                        Err(e) => {
                            warn!(player = %pubkey, "discarding datagram: {e}");
                            continue;
                        }
                    };

                    let decompressed_message = Compressed(message).decompress()?;

                    let instruction: ServerInstruction<<Q::UserLogic as DeformUserLogic>::Inputs> =
                        wincode::deserialize(&decompressed_message)
                            .map_err(|e| DeformError::Deserialize(e.to_string()))?;

                    match instruction {
                        ServerInstruction::BatchSetInputs(inputs) => {
                            // try_send here so we don't block if the channel is full
                            // FIX: use the error value to tell the client that the server is lagging??
                            let _ = match_sender.try_send(MatchMessage::Inputs { pubkey, inputs });
                        }
                    }
                }
            });
        }

        info!(player = %pubkey, "Client loop ended");
        Ok(())
    }

    /// Forwards both reliable and unreliable messages to the client, also checking if the match has finished (maybe move that out of here???)
    async fn forward_to_client(
        pubkey: Pubkey,
        connection: &Connection,
        control_send: &mut quinn::SendStream,
        internal_broadcast: &InternalServerBroadcast<Q>,
        fragmentor: &mut DatagramFragmentor<Q>,
    ) -> UserFacingResult<Q::UserLogic, bool> {
        let mut finished = false;

        match internal_broadcast {
            InternalServerBroadcast::SendDatagrams {
                lobby_state,
                player_inputs_buffers_len,
            } => {
                let player_input_buffer_len = match player_inputs_buffers_len.get(&pubkey) {
                    Some(player_input_buffer_len) => *player_input_buffer_len,
                    // TODO: error out instead?
                    None => 0,
                };

                let packet = StateUpdatePacket {
                    lobby_state: lobby_state.clone(),
                    player_input_buffer_len,
                };

                let message_bytes = wincode::serialize(&packet)
                    .map_err(|e| DeformError::Serialize(e.to_string()))?;

                fragmentor.send(&message_bytes)?;
            }
            InternalServerBroadcast::SendReliableMessage(msg) => {
                msg.write(control_send).await?;

                match msg {
                    ReliableMessage::Finish(_) => {
                        // Give a grace period, then close the connection
                        sleep(Duration::from_secs(5)).await;

                        connection.close(quinn::VarInt::from_u32(0), b"game_over");

                        finished = true;
                    }
                    _ => {}
                }
            }
        }

        Ok(finished)
    }
}
