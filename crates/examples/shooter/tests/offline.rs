//! End-to-end smoke test of the offline backend driving the real avian/tnua sim —
//! the same "always implement offline first" harness the skill recommends.
#![cfg(feature = "bin")]

use std::{collections::BTreeMap, time::Duration};

use deform_core::{
    Pubkey,
    accounts::lobby::{
        Lobby, LobbyMetadata, LobbyState, Network, PlayerStatus, Web2Server,
        not_started::LobbyNotStarted,
    },
};
use deform_offline::new_offline_client;
use shooter::shooter_logic::{ShooterGame, ShooterInputs, shooter_bot};
use tokio_util::sync::CancellationToken;

#[test]
fn offline_backend_runs_the_sim() {
    let me = Pubkey::new_from_array([1; 32]);
    let bot = Pubkey::new_from_array([255; 32]);

    let mut player_status = BTreeMap::new();
    player_status.insert(me, PlayerStatus::Ready);
    player_status.insert(bot, PlayerStatus::Ready);

    let lobby = Lobby {
        metadata: LobbyMetadata {
            id: 0,
            creator: me,
            network: Network::Web2(Web2Server::Localhost),
            bump: 0,
        },
        state: LobbyState::NotStarted(LobbyNotStarted { player_status }),
    };

    let cancellation_token = CancellationToken::new();
    let client = new_offline_client::<ShooterGame>(
        me,
        lobby,
        shooter_bot,
        16_667, // ~60 fps visual rate
        cancellation_token.clone(),
    )
    .expect("offline client should start");

    // Hold the trigger and walk forward while the bot does its thing.
    let mut inputs = ShooterInputs::default();
    inputs.move_z = 100;
    inputs.fire = true;

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut last_tick = 0;
    let mut saw_projectile = false;
    while std::time::Instant::now() < deadline {
        client.set_inputs(inputs.clone()).expect("send inputs");
        std::thread::sleep(Duration::from_millis(50));

        let state = client.read_state().expect("read state");
        if let LobbyState::Ongoing(ongoing) = &state.lobby.state {
            last_tick = ongoing.tick;
            saw_projectile |= !ongoing.tick_info.game_state.projectiles.is_empty();
            assert!(
                state.internal_error.is_ok(),
                "backend reported an error: {:?}",
                state.internal_error
            );
        }
        if last_tick > 40 && saw_projectile {
            break;
        }
    }

    assert!(
        last_tick > 20,
        "simulation should have ticked, got {last_tick}"
    );
    assert!(saw_projectile, "held fire should have spawned projectiles");

    client.shutdown();
}

/// Regression test for the offline backend's visual interpolation: the state
/// handed to the renderer must move smoothly between the last two sim ticks. A
/// stale interpolation origin (the bug this guards against: `previous_game_state`
/// frozen at the initial state) makes every entity oscillate between its spawn
/// position and its current one at tick rate — "everything shakes".
#[test]
fn visual_state_does_not_jump_backwards() {
    let me = Pubkey::new_from_array([3; 32]);
    let bot = Pubkey::new_from_array([254; 32]);

    let mut player_status = BTreeMap::new();
    player_status.insert(me, PlayerStatus::Ready);
    player_status.insert(bot, PlayerStatus::Ready);

    let lobby = Lobby {
        metadata: LobbyMetadata {
            id: 0,
            creator: me,
            network: Network::Web2(Web2Server::Localhost),
            bump: 0,
        },
        state: LobbyState::NotStarted(LobbyNotStarted { player_status }),
    };

    let cancellation_token = CancellationToken::new();
    // ~500 Hz visual rate: several visual samples per sim tick, like a
    // high-refresh monitor
    let client = new_offline_client::<ShooterGame>(
        me,
        lobby,
        |_, _, _| ShooterInputs::default(), // idle bot: nothing else moves me
        2_000,
        cancellation_token.clone(),
    )
    .expect("offline client should start");

    // Hold forward (yaw 0 => walking toward -Z, away from the spawn ring).
    let mut inputs = ShooterInputs::default();
    inputs.move_z = 100;

    // Let the player settle and get up to speed first.
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_millis(500) {
        client.set_inputs(inputs.clone()).expect("send inputs");
        std::thread::sleep(Duration::from_millis(10));
    }

    let mut samples: Vec<f32> = Vec::new();
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_millis(1_000) {
        client.set_inputs(inputs.clone()).expect("send inputs");
        {
            let state = client.read_state().expect("read state");
            if let LobbyState::Ongoing(ongoing) = &state.lobby.state {
                if let Some(ps) = ongoing.tick_info.game_state.players.get(&me) {
                    samples.push(ps.pos.z);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    assert!(samples.len() > 100, "expected many samples");
    let travelled = samples.first().unwrap() - samples.last().unwrap();
    assert!(
        travelled > 3.0,
        "should have walked several meters along -Z, moved {travelled}"
    );

    // Walking steadily forward, the visual position must never move backwards by
    // more than a smoothing-offset wiggle. The stale-origin bug produced jumps
    // back toward spawn as large as the whole distance travelled.
    let max_backwards = samples
        .windows(2)
        .map(|w| w[1] - w[0])
        .fold(0.0_f32, f32::max);
    assert!(
        max_backwards < 0.2,
        "visual position jumped backwards by {max_backwards} m between samples"
    );
}
