//! Bulletin chain storage: content-addressed block storage primitives
//! ([`storage`]), the CAR read/upload layer ([`upload`]) built on them, and
//! the upload signer with its cross-run safety state ([`signer`], [`inflight`]).

pub mod inflight;
pub mod signer;
pub mod storage;
pub mod upload;

pub use signer::{acquire_explicit_signer, acquire_pool_signer, UploadSigner};
pub use storage::{
    authorization, authorize_bulletin_account, bulletin_client, confirm_finalized, content_hash,
    raw_cid, store_block, store_blocks, Hashing, PreparedBlock, StoreOutcome, MAX_TRANSACTION_SIZE,
};
pub use upload::{read_car_prepared, store_car_file, store_prepared_blocks};
