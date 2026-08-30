use std::{collections::BTreeMap, str::FromStr};

use deform_core::{
    DeformUserLogic, Pubkey,
    accounts::lobby::{
        Lobby, LobbyMetadata, LobbyState, Network, PlayerStatus, Web2Server,
        not_started::LobbyNotStarted,
    },
};
use tokio_util::sync::CancellationToken;

use crate::{
    buffer::{ByteBuffer, json_error},
    client::{leak_client, pubkey_from_buffer},
};

/// Starts the offline backend and returns a leaked client handle.
///
/// There is no lobby account to read here, so the lobby is built locally: `players` is a
/// JSON array of base58 pubkeys whose **first entry is the creator**, all marked ready, and
/// `player` picks which of them the host drives. Everyone else is driven by `bot_fn`.
///
/// # Safety
/// Every [`ByteBuffer`] argument must point to initialized memory of its stated length for
/// the duration of the call.
pub unsafe fn new_offline_client<T: DeformUserLogic>(
    player: ByteBuffer,
    players: ByteBuffer,
    lobby_id: u64,
    bot_fn: fn(&T::GameState, &Pubkey, &T::Inputs) -> T::Inputs,
    visual_tick_micros: u64,
) -> ByteBuffer {
    let player = match unsafe { pubkey_from_buffer(&player, "player") } {
        Ok(player) => player,
        Err(e) => return json_error(e),
    };

    let players_json = match unsafe { players.as_str() } {
        Ok(json) => json,
        Err(e) => return json_error(format!("players are not utf-8: {e}")),
    };

    let players: Vec<String> = match serde_json::from_str(players_json) {
        Ok(players) => players,
        Err(e) => return json_error(format!("deserialize players: {e}")),
    };

    let Some(creator) = players.first() else {
        return json_error("players is empty");
    };

    let creator = match Pubkey::from_str(creator.trim()) {
        Ok(creator) => creator,
        Err(e) => return json_error(format!("creator is not a base58 pubkey: {e}")),
    };

    let mut player_status = BTreeMap::new();
    for pubkey in &players {
        match Pubkey::from_str(pubkey.trim()) {
            Ok(pubkey) => player_status.insert(pubkey, PlayerStatus::Ready),
            Err(e) => return json_error(format!("player {pubkey} is not a base58 pubkey: {e}")),
        };
    }

    if !player_status.contains_key(&player) {
        return json_error("player is not one of the players");
    }

    let lobby = Lobby::<T> {
        metadata: LobbyMetadata {
            id: lobby_id,
            creator,
            network: Network::Web2(Web2Server::Localhost),
            bump: 0,
        },
        state: LobbyState::NotStarted(LobbyNotStarted { player_status }),
    };

    match deform_offline::new_offline_client::<T>(
        player,
        lobby,
        bot_fn,
        visual_tick_micros,
        CancellationToken::new(),
    ) {
        Ok(client) => leak_client(client),
        Err(e) => json_error(e),
    }
}
