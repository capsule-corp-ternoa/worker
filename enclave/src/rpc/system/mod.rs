// This file is part of Substrate.

// Copyright (C) 2019-2021 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! System FRAME specific RPC methods.

pub extern crate alloc;
use alloc::{boxed::Box, vec::Vec};
use core::pin::Pin;

use codec::{self, Codec, Decode, Encode};
use sp_runtime::{
	generic::BlockId,
	traits,
};
use sp_core::{hexdisplay::HexDisplay, Bytes};


use log::*;

use std::sync::Arc;
use core::iter::Iterator;
use jsonrpc_core::futures::future::{ready, TryFutureExt, Future};
use sp_runtime::generic;

use crate::rpc::error::Error as StateRpcError;
use crate::top_pool::{
    error::Error as PoolError,
    error::IntoPoolError,
    primitives::{
        BlockHash, InPoolOperation, TrustedOperationPool, TrustedOperationSource, TxHash,
    },
};
use jsonrpc_core::{Error as RpcError, ErrorCode};

use crate::rsa3072;
use crate::state;

use substratee_stf::{
    AccountId, Getter, ShardIdentifier, Stf, TrustedCall, TrustedCallSigned, TrustedGetterSigned, Index,
};


use crate::aes;
use crate::attestation;
use crate::ed25519;
use crate::rpc;
use crate::top_pool;

use crate::{Timeout, WorkerRequest, WorkerResponse};
use log::*;

use sgx_tunittest::*;
use sgx_types::{sgx_status_t, size_t};

use substrate_api_client::utils::storage_key;
use substratee_worker_primitives::block::StatePayload;

use sp_core::{crypto::Pair, hashing::blake2_256, H256};

use crate::constants::{BLOCK_CONFIRMED, GETTERTIMEOUT, SUBSRATEE_REGISTRY_MODULE};

use std::string::String;

use crate::ipfs::IpfsContent;
use core::ops::Deref;
use std::fs::File;
use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};
use std::untrusted::time::SystemTimeEx;

use chain_relay::{Block, Header};
use sp_runtime::traits::Header as HeaderT;

use sgx_externalities::SgxExternalitiesTypeTrait;
use substratee_stf::StateTypeDiff as StfStateTypeDiff;
use jsonrpc_core::futures::executor;
use sp_core::ed25519 as spEd25519;
use substratee_stf::{TrustedGetter, TrustedOperation};

use rpc::author::{Author, AuthorApi};
use rpc::{api::FillerChainApi, basic_pool::BasicPool};

/// Future that resolves to account nonce.
pub type Result<T> = core::result::Result<T, RpcError>;

/// System RPC methods.
pub trait SystemApi {
	/// Returns the next valid index (aka nonce) for given account.
	///
	/// This method takes into consideration all pending transactions
	/// currently in the pool and if no transactions are found in the pool
	/// it fallbacks to query the index from the runtime (aka. state nonce).
	fn nonce(&self, encrypted_account: Vec<u8>, shard: ShardIdentifier) -> Result<Index>;
}

/// Error type of this RPC api.
pub enum Error {
	/// The transaction was not decodable.
	DecodeError,
	/// The call to state failed.
	StateError,
}

impl From<Error> for i64 {
	fn from(e: Error) -> i64 {
		match e {
			Error::StateError => 1,
			Error::DecodeError => 2,
		}
	}
}

/// An implementation of System-specific RPC methods on full client.
pub struct FullSystem<P> {
	pool: Arc<P>,
}

impl<P> FullSystem<P> {
	/// Create new `FullSystem` given client and transaction pool.
	pub fn new(pool: Arc<P>) -> Self {
		FullSystem {
			pool,
		}
	}
}

impl<P> SystemApi for FullSystem<&P>
where
	P: TrustedOperationPool + 'static,
{
	fn nonce(&self, encrypted_account: Vec<u8>, shard: ShardIdentifier) -> Result<Index> {
		if !state::exists(&shard) {
			//FIXME: Should this be an error? -> Issue error handling
			error!("Shard does not exists");
			return Ok(0 as Index)
		}
		// decrypt account
        let rsa_keypair = rsa3072::unseal_pair().unwrap();
        let account_vec: Vec<u8> = match rsa3072::decrypt(&encrypted_account.as_slice(), &rsa_keypair) {
            Ok(acc) => acc,
            Err(e) => return Err(RpcError {
				code: ErrorCode::ServerError(Error::DecodeError.into()),
				message: "Unable to query nonce.".into(),
				data: Some(format!("{:?}", e).into())
			})
        };
        // decode account
        let account = match AccountId::decode(&mut account_vec.as_slice()) {
            Ok(acc) => acc,
            Err(e) => return Err(RpcError {
				code: ErrorCode::ServerError(Error::DecodeError.into()),
				message: "Unable to query nonce.".into(),
				data: Some(format!("{:?}", e).into())
			})
        };

		let mut state = match state::load(&shard) {
			Ok(s) => s,
			Err(e) => {
				//FIXME: Should this be an error? -> Issue error handling
				error!("Shard could not be loaded");
				return Err(RpcError {
					code: ErrorCode::ServerError(Error::StateError.into()),
					message: "Unable to query nonce of current state.".into(),
					data: Some(format!("{:?}", e).into())
				})
			}
		};

		let nonce: Index = if let Some(nonce_encoded) = Stf::account_nonce(&mut state, account.clone()) {
			match Decode::decode(&mut nonce_encoded.as_slice()) {
				Ok(index) => index,
				Err(e) => {
					error!("Could not decode index");
					return Err(RpcError {
						code: ErrorCode::ServerError(Error::DecodeError.into()),
						message: "Unable to query nonce.".into(),
						data: Some(format!("{:?}", e).into())
					})
				},
			}
		} else {
			0 as Index
		};

		Ok(adjust_nonce(*self.pool, account, nonce, shard))
	}
}


/// Adjust account nonce from state, so that tx with the nonce will be
/// placed after all ready txpool transactions.
fn adjust_nonce<P>(
	pool: &P,
	account: AccountId,
	nonce: Index,
    shard: ShardIdentifier,
) -> Index where
	P: TrustedOperationPool,
{
	log::debug!(target: "rpc", "State nonce for {:?}: {}", account, nonce);
	// Now we need to query the transaction pool
	// and find transactions originating from the same sender.
	//
	// Since extrinsics are opaque to us, we look for them using
	// `provides` tag. And increment the nonce if we find a transaction
	// that matches the current one.
	let mut current_nonce: Index = nonce.clone();
	let mut current_tag = (account.clone(), nonce).encode();
	for tx in pool.ready(shard) {
		log::debug!(
			target: "rpc",
			"Current nonce to {}, checking {} vs {:?}",
			current_nonce,
			HexDisplay::from(&current_tag),
			tx.provides().iter().map(|x| format!("{}", HexDisplay::from(x))).collect::<Vec<_>>(),
		);
		// since transactions in `ready()` need to be ordered by nonce
		// it's fine to continue with current iterator.
		if tx.provides().get(0) == Some(&current_tag) {
			current_nonce += 1;
			current_tag = (account.clone(), current_nonce.clone()).encode();
		}
	}

	current_nonce
}

pub mod tests {
	use super::*;

	pub fn test_should_return_next_nonce_for_some_account() {
		// given
		// create top pool
		let api: Arc<FillerChainApi<Block>> = Arc::new(FillerChainApi::new());
		let tx_pool = BasicPool::create(Default::default(), api);

		let shard = ShardIdentifier::default();
		// ensure state starts empty
		state::init_shard(&shard).unwrap();
		Stf::init_state();

		// create account
		let pair_with_money = spEd25519::Pair::from_seed(b"12345678901234567890123456789012");

		let source = TrustedOperationSource::External;

		// encrypt account
		let rsa_pubkey = rsa3072::unseal_pubkey().unwrap();
		let mut encrypted_account: Vec<u8> = Vec::new();
			rsa_pubkey
				.encrypt_buffer(&pair_with_money.public().encode(), &mut encrypted_account)
				.unwrap();

		// create top call function
		let new_top_call = |nonce: Index| {
			let signer_pair = ed25519::unseal_pair().unwrap();
			let mrenclave = [0u8; 32];
			let call = TrustedCall::balance_set_balance(
				signer_pair.public().into(),
				signer_pair.public().into(),
				42,
				42,
			);
			let signed_call = call.sign(&signer_pair.into(), nonce, &mrenclave, &shard);
			signed_call.into_trusted_operation(true)
		};
		// Populate the pool
		let top0 = new_top_call(0);
		executor::block_on(tx_pool.submit_one(&BlockId::number(0), source, top0, shard)).unwrap();
		let top1 = new_top_call(1);
		executor::block_on(tx_pool.submit_one(&BlockId::number(0), source, top1, shard)).unwrap();

		let system = FullSystem::new(Arc::new(&tx_pool));

		// when
		let nonce = system.nonce(encrypted_account, shard);

		// then
		assert_eq!(nonce.unwrap(), 2);
	}
}