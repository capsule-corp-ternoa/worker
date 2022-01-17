/*
	Copyright 2021 Integritee AG and Supercomputing Systems AG

	Licensed under the Apache License, Version 2.0 (the "License");
	you may not use this file except in compliance with the License.
	You may obtain a copy of the License at

		http://www.apache.org/licenses/LICENSE-2.0

	Unless required by applicable law or agreed to in writing, software
	distributed under the License is distributed on an "AS IS" BASIS,
	WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
	See the License for the specific language governing permissions and
	limitations under the License.

*/

use crate::{
	error::{Error, Result},
	global_components::GLOBAL_PARENTCHAIN_IMPORT_DISPATCHER_COMPONENT,
	ocall::OcallApi,
	sync::{EnclaveLock, EnclaveStateRWLock},
};
use codec::Encode;
use itc_parentchain::{
	block_import_dispatcher::triggered_dispatcher::TriggerParentchainBlockImport,
	light_client::{
		concurrent_access::ValidatorAccess, BlockNumberOps, LightClientState, NumberFor, Validator,
		ValidatorAccessor,
	},
};
use itp_component_container::ComponentGetter;
use itp_extrinsics_factory::{CreateExtrinsics, ExtrinsicsFactory};
use itp_nonce_cache::GLOBAL_NONCE_CACHE;
use itp_ocall_api::{EnclaveOnChainOCallApi, EnclaveSidechainOCallApi};
use itp_settings::sidechain::SLOT_DURATION;
use itp_sgx_crypto::{AesSeal, Ed25519Seal};
use itp_sgx_io::SealedIO;
use itp_stf_executor::executor::StfExecutor;
use itp_stf_state_handler::{query_shard_state::QueryShardState, GlobalFileStateHandler};
use itp_storage_verifier::GetStorageVerified;
use itp_time_utils::duration_now;
use itp_types::{Block, OpaqueCall, H256};
use its_sidechain::{
	aura::{proposer_factory::ProposerFactory, Aura, SlotClaimStrategy},
	block_composer::BlockComposer,
	consensus_common::{Environment, Error as ConsensusError},
	primitives::{
		traits::{Block as SidechainBlockT, ShardIdentifierFor, SignedBlock},
		types::block::SignedBlock as SignedSidechainBlock,
	},
	slots::{sgx::LastSlotSeal, yield_next_slot, PerShardSlotWorkerScheduler, SlotInfo},
	top_pool_executor::TopPoolOperationHandler,
	top_pool_rpc_author::global_author_container::GLOBAL_RPC_AUTHOR_COMPONENT,
	validateer_fetch::ValidateerFetch,
};
use log::*;
use sgx_types::sgx_status_t;
use sp_core::Pair;
use sp_runtime::{
	generic::SignedBlock as SignedParentchainBlock, traits::Block as BlockTrait, MultiSignature,
};
use std::{sync::Arc, vec::Vec};

#[no_mangle]
pub unsafe extern "C" fn execute_trusted_calls() -> sgx_status_t {
	if let Err(e) = execute_top_pool_trusted_calls_internal() {
		return e.into()
	}

	sgx_status_t::SGX_SUCCESS
}

/// Internal [`execute_trusted_calls`] function to be able to use the `?` operator.
///
/// Executes `Aura::on_slot() for `slot` if it is this enclave's `Slot`.
///
/// This function makes an ocall that does the following:
///
/// *   Import all pending parentchain blocks.
/// *   Sends sidechain `confirm_block` xt's with the produced sidechain blocks.
/// *   Gossip produced sidechain blocks to peer validateers.
fn execute_top_pool_trusted_calls_internal() -> Result<()> {
	// We acquire lock explicitly (variable binding), since '_' will drop the lock after the statement.
	// See https://medium.com/codechain/rust-underscore-does-not-bind-fec6a18115a8
	let _enclave_write_lock = EnclaveLock::write_all()?;

	let parentchain_import_dispatcher = GLOBAL_PARENTCHAIN_IMPORT_DISPATCHER_COMPONENT
		.get()
		.ok_or(Error::ComponentNotInitialized)?;

	let validator_access = ValidatorAccessor::<Block>::default();

	// This gets the latest imported block. We accept that all of AURA, up until the block production
	// itself, will  operate on a parentchain block that is potentially outdated by one block
	// (in case we have a block in the queue, but not imported yet).
	let (latest_parentchain_header, genesis_hash) = validator_access.execute_on_validator(|v| {
		let latest_parentchain_header = v.latest_finalized_header(v.num_relays())?;
		let genesis_hash = v.genesis_hash(v.num_relays())?;
		Ok((latest_parentchain_header, genesis_hash))
	})?;

	let authority = Ed25519Seal::unseal()?;
	let state_key = AesSeal::unseal()?;

	let rpc_author = GLOBAL_RPC_AUTHOR_COMPONENT.get().ok_or_else(|| {
		error!("Failed to retrieve author mutex. Maybe it's not initialized?");
		Error::ComponentNotInitialized
	})?;

	let state_handler = Arc::new(GlobalFileStateHandler);
	let stf_executor = Arc::new(StfExecutor::new(Arc::new(OcallApi), state_handler.clone()));
	let extrinsics_factory =
		ExtrinsicsFactory::new(genesis_hash, authority.clone(), GLOBAL_NONCE_CACHE.clone());

	let top_pool_executor = Arc::new(
		TopPoolOperationHandler::<Block, SignedSidechainBlock, _>::new(rpc_author.clone()),
	);

	let block_composer = Arc::new(BlockComposer::new(authority.clone(), state_key, rpc_author));

	match yield_next_slot(
		duration_now(),
		SLOT_DURATION,
		latest_parentchain_header,
		&mut LastSlotSeal,
	)? {
		Some(slot) => {
			let shards = state_handler.list_shards()?;
			let env = ProposerFactory::<Block, _, _, _>::new(
				top_pool_executor,
				stf_executor,
				block_composer,
			);

			let (blocks, opaque_calls) = exec_aura_on_slot::<_, _, SignedSidechainBlock, _, _, _>(
				slot,
				authority,
				OcallApi,
				parentchain_import_dispatcher,
				env,
				shards,
			)?;

			// Drop lock as soon as we don't need it anymore.
			drop(_enclave_write_lock);

			send_blocks_and_extrinsics::<Block, _, _, _, _>(
				blocks,
				opaque_calls,
				OcallApi,
				&validator_access,
				&extrinsics_factory,
			)?
		},
		None => {
			debug!("No slot yielded. Skipping block production.");
			return Ok(())
		},
	};

	Ok(())
}

/// Executes aura for the given `slot`.
pub(crate) fn exec_aura_on_slot<Authority, PB, SB, OCallApi, PEnvironment, BlockImportTrigger>(
	slot: SlotInfo<PB>,
	authority: Authority,
	ocall_api: OCallApi,
	block_import_trigger: Arc<BlockImportTrigger>,
	proposer_environment: PEnvironment,
	shards: Vec<ShardIdentifierFor<SB>>,
) -> Result<(Vec<SB>, Vec<OpaqueCall>)>
where
	PB: BlockTrait<Hash = H256>,
	SB: SignedBlock<Public = Authority::Public, Signature = MultiSignature> + 'static, // Setting the public type is necessary due to some non-generic downstream code.
	SB::Block: SidechainBlockT<ShardIdentifier = H256, Public = Authority::Public>,
	SB::Signature: From<Authority::Signature>,
	Authority: Pair<Public = sp_core::ed25519::Public>,
	Authority::Public: Encode,
	OCallApi: ValidateerFetch + GetStorageVerified + Send + 'static,
	NumberFor<PB>: BlockNumberOps,
	PEnvironment: Environment<PB, SB, Error = ConsensusError> + Send + Sync,
	BlockImportTrigger: TriggerParentchainBlockImport<SignedParentchainBlock<PB>>,
{
	log::info!("[Aura] Executing aura for slot: {:?}", slot);

	let mut aura = Aura::<_, PB, SB, PEnvironment, _, _>::new(
		authority,
		ocall_api,
		block_import_trigger,
		proposer_environment,
	)
	.with_claim_strategy(SlotClaimStrategy::Always)
	.with_allow_delayed_proposal(true);

	let (blocks, xts): (Vec<_>, Vec<_>) =
		PerShardSlotWorkerScheduler::on_slot(&mut aura, slot, shards)
			.into_iter()
			.map(|r| (r.block, r.parentchain_effects))
			.unzip();

	let opaque_calls: Vec<OpaqueCall> = xts.into_iter().flatten().collect();
	Ok((blocks, opaque_calls))
}

/// Gossips sidechain blocks to fellow peers and sends opaque calls as extrinsic to the parentchain.
pub(crate) fn send_blocks_and_extrinsics<PB, SB, OCallApi, ValidatorAccessor, ExtrinsicsFactory>(
	blocks: Vec<SB>,
	opaque_calls: Vec<OpaqueCall>,
	ocall_api: OCallApi,
	validator_access: &ValidatorAccessor,
	extrinsics_factory: &ExtrinsicsFactory,
) -> Result<()>
where
	PB: BlockTrait,
	SB: SignedBlock + 'static,
	OCallApi: EnclaveSidechainOCallApi + EnclaveOnChainOCallApi,
	ValidatorAccessor: ValidatorAccess<PB> + Send + Sync + 'static,
	NumberFor<PB>: BlockNumberOps,
	ExtrinsicsFactory: CreateExtrinsics,
{
	ocall_api.propose_sidechain_blocks(blocks)?;

	let xts = extrinsics_factory.create_extrinsics(opaque_calls.as_slice())?;

	validator_access.execute_mut_on_validator(|v| v.send_extrinsics(&ocall_api, xts))?;
	Ok(())
}
