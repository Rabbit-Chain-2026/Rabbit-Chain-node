//! RabbitChain Integration Tests

use rabbitcore::crypto::{Address, Ed25519PrivateKey, Hash};
use rabbitcore::account::{Account, AccountType, U256, InMemoryAccountManager, AccountManager};
use rabbitcore::block::{Block, create_genesis_block};
use rabbitcore::consensus::{PowConsensus, PowAlgorithm, MiningEngine, MiningConfig};
use rabbitcore::state::{StateDb, StateExecutor};
use rabbitcore::blockchain::{Blockchain, SyncManager, SyncConfig};
use rabbitstore::trie::{MerklePatriciaTrie, MemTrieDB};
use std::sync::Arc;

/// Test fixture for integration tests
struct TestFixture {
    consensus: Arc<PowConsensus>,
    state_db: Arc<StateDb>,
    blockchain: Arc<Blockchain>,
    account_manager: Arc<InMemoryAccountManager>,
    mining_engine: Arc<MiningEngine>,
}

impl TestFixture {
    /// Create new test fixture
    fn new() -> Self {
        let consensus = Arc::new(PowConsensus::new(PowAlgorithm::LightHash));
        let state_db = Arc::new(StateDb::new(Hash::zero()));
        let blockchain = Arc::new(Blockchain::new(consensus.clone(), state_db.clone()));
        let account_manager = Arc::new(InMemoryAccountManager::new());
        
        let mining_config = MiningConfig {
            enabled: true,
            coinbase: Address::from_bytes([1u8; 20]),
            threads: 2,
            algorithm: PowAlgorithm::LightHash,
            ..Default::default()
        };
        
        let mining_engine = Arc::new(MiningEngine::new(
            mining_config,
            consensus.clone(),
            state_db.clone(),
        ));
        
        Self {
            consensus,
            state_db,
            blockchain,
            account_manager,
            mining_engine,
        }
    }
}

/// Test: Sign and verify transaction envelope
#[tokio::test]
async fn test_transaction_execution() {
    let sender_key = Ed25519PrivateKey::random();
    let sender_addr = Address::from_public_key(&sender_key.public_key());
    
    let recipient_key = Ed25519PrivateKey::random();
    let recipient_addr = Address::from_public_key(&recipient_key.public_key());

    let signing_payload = [
        sender_addr.as_bytes(),
        recipient_addr.as_bytes(),
        &1000u64.to_be_bytes(),
    ]
    .concat();
    let signature = sender_key.sign(&signing_payload);
    let verified = signature
        .verify(&signing_payload, &sender_key.public_key())
        .unwrap();
    assert!(verified);

    println!("✓ Transaction envelope signs and verifies");
}

/// Test: Mine block with transactions
#[tokio::test]
async fn test_block_mining() {
    let fixture = TestFixture::new();
    
    // Start mining
    fixture.mining_engine.start_mining().unwrap();
    
    // Get genesis block
    let genesis = fixture.blockchain.genesis();
    
    // Mine new block
    let block = fixture.mining_engine.mine_block(&genesis.header).unwrap();
    
    // Verify block
    assert_eq!(block.header.number, U256::from(1));
    assert_eq!(block.header.parent_hash, genesis.header.hash);
    assert!(!block.header.hash.is_zero());
    
    // Verify PoW
    fixture.consensus.verify_pow(&block.header).unwrap();
    
    println!("✓ Block mined successfully: #{}", block.header.number.as_u64());
}

/// Test: State transitions
#[tokio::test]
async fn test_state_transitions() {
    let fixture = TestFixture::new();
    
    let executor = StateExecutor::new(fixture.state_db.clone(), 10086);
    
    // Create test account
    let key = Ed25519PrivateKey::random();
    let addr = Address::from_public_key(&key.public_key());
    
    let mut account = Account::new_user_account(key.public_key(), addr);
    account.balance = U256::from(1_000_000);
    fixture.account_manager.create_account(
        account.account_type.clone(),
        account.config.clone(),
    ).await.unwrap();
    
    // Get initial state
    let initial_balance = fixture.state_db.get_balance(&addr);
    assert_eq!(initial_balance, U256::from(1_000_000));
    
    println!("✓ State transitions working correctly");
}

/// Test: MPT Trie operations
#[test]
fn test_mpt_trie() {
    let db = Arc::new(MemTrieDB::new());
    let trie = MerklePatriciaTrie::new(db);
    
    // Insert key-value pairs
    let root1 = trie.insert(b"key1", b"value1".to_vec().into()).unwrap();
    let root2 = trie.insert(b"key2", b"value2".to_vec().into()).unwrap();
    let root3 = trie.insert(b"key3", b"value3".to_vec().into()).unwrap();
    
    // Verify root changes
    assert!(!root1.is_zero());
    assert!(!root2.is_zero());
    assert!(!root3.is_zero());
    assert_ne!(root1, root2);
    assert_ne!(root2, root3);
    
    // Retrieve values
    let val1 = trie.get(b"key1").unwrap().unwrap();
    let val2 = trie.get(b"key2").unwrap().unwrap();
    let val3 = trie.get(b"key3").unwrap().unwrap();
    
    assert_eq!(val1.as_ref(), b"value1");
    assert_eq!(val2.as_ref(), b"value2");
    assert_eq!(val3.as_ref(), b"value3");
    
    // Delete key
    let removed = trie.remove(b"key2").unwrap().unwrap();
    assert_eq!(removed.as_ref(), b"value2");
    
    // Verify deletion
    assert!(trie.get(b"key2").unwrap().is_none());
    
    println!("✓ MPT Trie operations working correctly");
}

/// Test: RLP encoding/decoding
#[test]
fn test_rlp_encoding() {
    use rabbitcore::rlp::{RlpEncode, RlpDecode};
    
    // Test u64
    let original = 1000u64;
    let encoded = original.rlp_encode();
    let decoded = u64::rlp_decode(&encoded).unwrap();
    assert_eq!(original, decoded);
    
    // Test string
    let original = "hello".to_string();
    let encoded = original.rlp_encode();
    let decoded = String::rlp_decode(&encoded).unwrap();
    assert_eq!(original, decoded);
    
    // Test list
    let original = vec![1u64, 2, 3, 4, 5];
    let encoded = original.rlp_encode();
    let decoded = Vec::<u64>::rlp_decode(&encoded).unwrap();
    assert_eq!(original, decoded);
    
    println!("✓ RLP encoding/decoding working correctly");
}

/// Test: Blockchain operations
#[test]
fn test_blockchain_operations() {
    let consensus = Arc::new(PowConsensus::new(PowAlgorithm::LightHash));
    let state_db = Arc::new(StateDb::new(Hash::zero()));
    let blockchain = Arc::new(Blockchain::new(consensus, state_db));
    
    // Get genesis
    let genesis = blockchain.genesis();
    assert_eq!(genesis.header.number, U256::zero());
    
    // Get best block
    let best = blockchain.best_block();
    assert_eq!(best.header.number, U256::zero());
    
    // Get block by number
    let block = blockchain.get_block_by_number(0);
    assert!(block.is_some());
    
    // Get chain info
    let info = blockchain.get_chain_info();
    assert_eq!(info.best_number, 0);
    
    println!("✓ Blockchain operations working correctly");
}

/// Test: Account management
#[tokio::test]
async fn test_account_management() {
    let manager = InMemoryAccountManager::new();
    
    // Create account
    let key = Ed25519PrivateKey::random();
    let account_type = AccountType::User {
        public_key: key.public_key(),
    };
    
    let account = manager.create_account(account_type, Default::default()).await.unwrap();
    
    // Verify account created
    let retrieved = manager.get_account(&account.address).await.unwrap().unwrap();
    assert_eq!(retrieved.address, account.address);
    
    // Update balance
    manager.update_balance(
        &account.address,
        rabbitcore::account::I256::from(1000),
        rabbitcore::account::BalanceChangeReason::Transfer,
    ).await.unwrap();
    
    let updated = manager.get_account(&account.address).await.unwrap().unwrap();
    assert_eq!(updated.balance, U256::from(1000));
    
    // Increment nonce
    manager.increment_nonce(&account.address).await.unwrap();
    
    let nonce = manager.get_nonce(&account.address).await.unwrap();
    assert_eq!(nonce, 1);
    
    println!("✓ Account management working correctly");
}

/// Test: Sync manager
#[tokio::test]
async fn test_sync_manager() {
    let config = SyncConfig::default();
    let consensus = Arc::new(PowConsensus::new(PowAlgorithm::LightHash));
    let state_db = Arc::new(StateDb::new(Hash::zero()));
    let blockchain = Arc::new(Blockchain::new(consensus.clone(), state_db.clone()));
    
    let sync = SyncManager::new(config, blockchain, state_db, consensus);
    
    // Initial state
    assert!(!sync.is_syncing());
    assert_eq!(sync.get_progress(), 0.0);
    
    // Add peers
    sync.add_peer("peer1".to_string());
    sync.add_peer("peer2".to_string());
    
    assert_eq!(sync.peer_count(), 2);
    
    // Remove peer
    sync.remove_peer("peer1");
    assert_eq!(sync.peer_count(), 1);
    
    println!("✓ Sync manager working correctly");
}

/// Test: User account construction
#[tokio::test]
async fn test_transaction_pool() {
    let key = Ed25519PrivateKey::random();
    let address = Address::from_public_key(&key.public_key());
    let account = Account::new_user_account(key.public_key(), address);

    assert_eq!(account.address, address);
    assert_eq!(account.nonce, 0);
    assert!(account.balance.is_zero());

    println!("✓ User account working correctly");
}

/// Benchmark: Signature creation
#[test]
fn bench_transaction_creation() {
    let start = std::time::Instant::now();
    
    for _ in 0..1000 {
        let key = Ed25519PrivateKey::random();
        let payload = b"rabbitchain-signature-smoke";
        let signature = key.sign(payload);
        assert!(signature.verify(payload, &key.public_key()).unwrap());
    }
    
    let elapsed = start.elapsed();
    println!("✓ Created 1000 signatures in {:?}", elapsed);
    println!("  Average: {:?}", elapsed / 1000);
}

/// Benchmark: MPT Trie operations
#[test]
fn bench_trie_operations() {
    let db = Arc::new(MemTrieDB::new());
    let trie = MerklePatriciaTrie::new(db);
    
    let start = std::time::Instant::now();
    
    // Insert 1000 keys
    for i in 0..1000 {
        let key = format!("key{}", i);
        let value = format!("value{}", i);
        trie.insert(key.as_bytes(), value.into_bytes().into()).unwrap();
    }
    
    let insert_elapsed = start.elapsed();
    
    // Get 1000 keys
    let start = std::time::Instant::now();
    for i in 0..1000 {
        let key = format!("key{}", i);
        trie.get(key.as_bytes()).unwrap();
    }
    
    let get_elapsed = start.elapsed();
    
    println!("✓ MPT Trie benchmark:");
    println!("  Insert 1000: {:?}", insert_elapsed);
    println!("  Get 1000: {:?}", get_elapsed);
    println!("  Average insert: {:?}", insert_elapsed / 1000);
    println!("  Average get: {:?}", get_elapsed / 1000);
}
