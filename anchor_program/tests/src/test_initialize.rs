use anchor_client::{
    CommitmentConfig,
    Client, Cluster,
};
use solana_keypair::{read_keypair_file};
use solana_pubkey::Pubkey;

#[test]
fn test_initialize() {
    let program_id = "D3m2Wjs5kCgXWaoJxAuZzyTZpvwjEqrhckxJBSs3THfV";
    let anchor_wallet = std::env::var("ANCHOR_WALLET").unwrap();
    let payer = read_keypair_file(&anchor_wallet).unwrap();

    let client = Client::new_with_options(Cluster::Localnet, &payer, CommitmentConfig::confirmed());
    let program_id = Pubkey::try_from(program_id).unwrap();
    let program = client.program(program_id).unwrap();

    let tx = program
        .request()
        .accounts(anchor_program::accounts::Initialize {})
        .args(anchor_program::instruction::Initialize {})
        .send()
        .expect("");

    println!("Your transaction signature {}", tx);
}
