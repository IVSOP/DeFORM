use std::time::Duration;

use better_tokio_select::tokio_select;
use deform_core::{DeformError, DeformUserLogic, Pubkey, error::UserFacingResult};
use quinn::Connection;
use tokio::{
    sync::{broadcast, mpsc},
    time::sleep,
};
use tracing::info;

use crate::{
    DeformQuicLogic, ReliableMessage, UnreliableServerInstruction,
    server::{
        DeformQuicServer,
        matches::{InternalServerResponse, MatchMessage},
    },
};

impl<Q: DeformQuicLogic> DeformQuicServer<Q> {
    // NOTE: Errors from client_loop get propagated backward, closing the connection and sending an error message to the client
    pub async fn client_loop(
        pubkey: Pubkey,
        match_sender: mpsc::Sender<MatchMessage<Q::UserLogic>>,
        connection: Connection,
        control_send: &mut quinn::SendStream,
        mut state_receiver: broadcast::Receiver<InternalServerResponse<Q>>,
    ) -> UserFacingResult<Q::UserLogic> {
        match_sender
            .send(MatchMessage::PlayerJoined { pubkey })
            .await
            .map_err(|_| DeformError::ChannelClosed)?;

        loop {
            tokio_select!(match .. {
                .. if let internal_response = state_receiver.recv() => {
                    match internal_response {
                        Ok(internal_response) => {
                            let finished = Self::forward_to_client(
                                &connection,
                                control_send,
                                internal_response,
                            )
                            .await?;

                            if finished {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            Err(DeformError::ChannelClosed)?;
                        }
                        Err(_) => continue,
                    }
                }
                .. if let datagram = connection.read_datagram() => {
                    let datagram = datagram.map_err(|e| DeformError::Connection(e.to_string()))?;
                    let instruction: UnreliableServerInstruction<
                        <Q::UserLogic as DeformUserLogic>::Inputs,
                    > = wincode::deserialize(&datagram)
                        .map_err(|e| DeformError::Deserialize(e.to_string()))?;

                    match instruction {
                        UnreliableServerInstruction::BatchSetInputs(inputs) => {
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
        connection: &Connection,
        control_send: &mut quinn::SendStream,
        internal_response: InternalServerResponse<Q>,
    ) -> UserFacingResult<Q::UserLogic, bool> {
        let mut finished = false;

        match internal_response {
            InternalServerResponse::SendDatagram(msg) => {
                connection
                    .send_datagram(msg.0.into())
                    .map_err(|e| DeformError::Connection(e.to_string()))?;
            }
            InternalServerResponse::SendReliableMessage(msg) => {
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
