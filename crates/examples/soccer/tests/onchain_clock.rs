use deform_core::{
    DeformUserLogic,
    accounts::lobby::{DevnetRegion, LocalRegion, ValidatorNetwork},
};
use soccer::soccer_logic::SoccerGame;

fn devnet() -> ValidatorNetwork {
    ValidatorNetwork::Devnet(DevnetRegion::EU)
}

#[test]
fn hosted_devnet_uses_measured_ten_millisecond_slots() {
    assert_eq!(SoccerGame::get_micros_per_slot(&devnet()), 10_000);
}

#[test]
fn one_second_on_devnet_advances_one_second_of_game_time() {
    // The EU RPC reports 6000 slots / 60 seconds: 100 slots per second.
    let ticks = SoccerGame::ticks_since_slot(Some(0), 100, &devnet());
    assert_eq!(ticks, 1_000_000 / SoccerGame::TICK_RATE_MICROS);
}

#[test]
fn extra_crank_calls_do_not_speed_up_the_game() {
    let mut last_slot = Some(0);
    let mut ticks = 0;
    for current_slot in 1..=100 {
        let due = SoccerGame::ticks_since_slot(last_slot, current_slot, &devnet());
        // Match the on-chain handler: only store a new slot when time advances.
        if due > 0 {
            ticks += due;
            last_slot = Some(current_slot);
        }
    }
    assert_eq!(ticks, SoccerGame::ticks_since_slot(Some(0), 100, &devnet()));
    assert_eq!(ticks, 1_000_000 / SoccerGame::TICK_RATE_MICROS);
}

#[test]
fn fractional_ticks_survive_irregular_crank_intervals() {
    let slots = [0, 3, 4, 7, 12, 13, 18, 31, 99, 100];
    let ticks: u64 = slots
        .windows(2)
        .map(|pair| SoccerGame::ticks_since_slot(Some(pair[0]), pair[1], &devnet()))
        .sum();
    assert_eq!(ticks, SoccerGame::ticks_since_slot(Some(0), 100, &devnet()));
}

#[cfg(feature = "20hz")]
#[test]
fn a_fifty_millisecond_crank_advances_one_tick_on_devnet() {
    assert_eq!(
        SoccerGame::ticks_since_slot(Some(639_000_000), 639_000_005, &devnet()),
        1
    );
    assert_eq!(
        SoccerGame::ticks_since_slot(Some(639_000_000), 639_000_004, &devnet()),
        0
    );
}

#[test]
fn localhost_keeps_its_fifty_millisecond_slot_clock() {
    let local = ValidatorNetwork::Localhost(LocalRegion::Local);
    assert_eq!(SoccerGame::get_micros_per_slot(&local), 50_000);
    assert_eq!(
        SoccerGame::ticks_since_slot(Some(0), 20, &local),
        SoccerGame::ticks_since_slot(Some(0), 100, &devnet())
    );
}

#[test]
fn duplicate_slots_and_backward_slots_do_not_advance_time() {
    assert_eq!(
        SoccerGame::ticks_since_slot(Some(u64::MAX), u64::MAX, &devnet()),
        0
    );
    assert_eq!(SoccerGame::ticks_since_slot(Some(100), 99, &devnet()), 0);
    assert_eq!(
        SoccerGame::ticks_since_slot(None, 639_000_000, &devnet()),
        1
    );
}

#[cfg(feature = "foc")]
#[test]
fn foc_accepts_the_devnet_slot_clock_during_client_startup() {
    use std::{collections::BTreeMap, sync::Arc};

    use deform_core::{
        DeformError, Pubkey,
        accounts::lobby::{
            Lobby, LobbyMetadata, LobbyState, Network, not_started::LobbyNotStarted,
        },
        error::UserFacingError,
    };
    use soccer::{soccer_logic::SoccerFocLogic, solana::anchor_client::SoccerAnchorClient};
    use solana_sdk::{signature::Keypair, signer::Signer};
    use tokio_util::sync::CancellationToken;

    for network in [devnet(), ValidatorNetwork::Localhost(LocalRegion::Local)] {
        let slot_time_micros = SoccerGame::get_micros_per_slot(&network);
        let keypair = Arc::new(Keypair::new());
        let lobby = Lobby {
            metadata: LobbyMetadata {
                id: 0,
                creator: Pubkey::new_from_array(keypair.pubkey().to_bytes()),
                network: Network::FullyOnChain(network),
                bump: 0,
            },
            state: LobbyState::NotStarted(LobbyNotStarted {
                player_status: BTreeMap::new(),
            }),
        };

        let result = deform_foc::new_foc_client::<SoccerFocLogic>(
            "http://unused.invalid".into(),
            // An invalid URL fails locally, proving startup reaches connection
            // setup without contacting a validator or sending a transaction.
            "invalid websocket URL".into(),
            keypair,
            SoccerAnchorClient,
            lobby,
            16_667,
            slot_time_micros,
            CancellationToken::new(),
        );
        assert!(
            matches!(
                result,
                Err(UserFacingError::Deform(DeformError::Connection(ref message)))
                    if message.starts_with("websocket connect to ")
            ),
            "valid slot and game clocks must reach WebSocket setup"
        );
    }
}
