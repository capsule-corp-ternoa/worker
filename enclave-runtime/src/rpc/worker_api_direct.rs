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

use crate::{attestation::create_ra_report_and_signature, ocall::OcallApi};
use codec::{Decode, Encode};
use core::result::Result;
use itp_ocall_api::EnclaveOnChainOCallApi;
use itp_sgx_crypto::Rsa3072Seal;
use itp_sgx_io::SealedIO;
use itp_types::{
	AccountId, DirectRequestStatus, RetrieveNftSecretRequest, RpcReturnValue, SignedRequest,
	StoreNftSecretRequest, H256,
};
use its_sidechain::{
	primitives::types::SignedBlock,
	rpc_handler::{direct_top_pool_api, import_block_api},
	top_pool_rpc_author::traits::AuthorApi,
};
use jsonrpc_core::{serde_json::json, Error, IoHandler, Params, Value};
use sgx_types::sgx_quote_sign_type_t;
use std::{borrow::ToOwned, format, str, string::String, sync::Arc, vec::Vec};
use ternoa_sgx_nft::NftDbSeal;

fn compute_encoded_return_error(error_msg: &str) -> Vec<u8> {
	RpcReturnValue::from_error_message(error_msg).encode()
}

fn get_all_rpc_methods_string(io_handler: &IoHandler) -> String {
	let method_string = io_handler
		.iter()
		.map(|rp_tuple| rp_tuple.0.to_owned())
		.collect::<Vec<String>>()
		.join(", ");

	format!("methods: [{}]", method_string)
}

pub fn public_api_rpc_handler<R>(rpc_author: Arc<R>) -> IoHandler
where
	R: AuthorApi<H256, H256> + Send + Sync + 'static,
{
	let io = IoHandler::new();

	// Add direct TOP pool rpc methods
	let mut io = direct_top_pool_api::add_top_pool_direct_rpc_methods(rpc_author, io);

	// author_getShieldingKey
	let rsa_pubkey_name: &str = "author_getShieldingKey";
	io.add_sync_method(rsa_pubkey_name, move |_: Params| {
		let rsa_pubkey = match Rsa3072Seal::unseal_pubkey() {
			Ok(key) => key,
			Err(status) => {
				let error_msg: String = format!("Could not get rsa pubkey due to: {}", status);
				return Ok(json!(compute_encoded_return_error(error_msg.as_str())))
			},
		};

		let rsa_pubkey_json = match serde_json::to_string(&rsa_pubkey) {
			Ok(k) => k,
			Err(x) => {
				let error_msg: String =
					format!("[Enclave] can't serialize rsa_pubkey {:?} {}", rsa_pubkey, x);
				return Ok(json!(compute_encoded_return_error(error_msg.as_str())))
			},
		};
		let json_value =
			RpcReturnValue::new(rsa_pubkey_json.encode(), false, DirectRequestStatus::Ok);
		Ok(json!(json_value.encode()))
	});

	// ra
	let ra_name: &str = "ra_test";
	io.add_sync_method(ra_name, |_: Params| {
		let sign_type = sgx_quote_sign_type_t::SGX_LINKABLE_SIGNATURE;
		let (key_der, cert_der) = match create_ra_report_and_signature(sign_type, &OcallApi, false)
		{
			Ok(r) => r,
			Err(e) => {
				let error_msg = format!("Failed to create ra report and signature due to: {}", e);
				return Ok(json!(compute_encoded_return_error(error_msg.as_str())))
			},
		};

		Ok(json!((key_der, cert_der).encode()))
	});

	// nft_storeSecret
	let nft_store_secret_name: &str = "nft_storeSecret";
	io.add_sync_method(nft_store_secret_name, |params: Params| {
		let encoded_params = params.parse::<Vec<u8>>()?;
		let signed_req =
			SignedRequest::<StoreNftSecretRequest>::decode(&mut encoded_params.as_slice())
				.map_err(|_| Error::invalid_request())?;

		let req = signed_req
			.get_request()
			.ok_or(Error::invalid_params("Invalid request signature"))?;

		let owner_id: AccountId = OcallApi.get_nft_owner(req.nft_id).map_err(|_| {
			Error::invalid_params(format!("NFT with id '{}' doesn't exist on chain", req.nft_id))
		})?;

		if owner_id != signed_req.signer.into() {
			return Err(Error::invalid_params("Request sender is not the owner of the NFT"))
		}

		let mut db = NftDbSeal::unseal().map_err(|_| Error::internal_error())?;

		db.upsert_sorted(req.nft_id, req.secret);

		NftDbSeal::seal(db).map_err(|_| Error::internal_error())?;

		Ok(Value::Null)
	});

	// nft_retrieveSecret
	let nft_retrieve_secret_name: &str = "nft_retrieveSecret";
	io.add_sync_method(nft_retrieve_secret_name, |params: Params| {
		let encoded_params = params.parse::<Vec<u8>>()?;
		let signed_req =
			SignedRequest::<RetrieveNftSecretRequest>::decode(&mut encoded_params.as_slice())
				.map_err(|_| Error::invalid_request())?;

		let req = signed_req
			.get_request()
			.ok_or(Error::invalid_params("Invalid request signature"))?;

		let owner_id: AccountId = OcallApi.get_nft_owner(req.nft_id).map_err(|_| {
			Error::invalid_params(format!("NFT with id '{}' doesn't exist on chain", req.nft_id))
		})?;

		if owner_id != signed_req.signer.into() {
			return Err(Error::invalid_params("Request sender is not the owner of the NFT"))
		}

		let mut db = NftDbSeal::unseal().map_err(|_| Error::internal_error())?;

		let secret = db.get(req.nft_id).map_err(|_| {
			Error::invalid_params(format!(
				"There is no secret stored for NFT with id '{}'",
				req.nft_id
			))
		})?;

		Ok(secret.into())
	});

	// chain_subscribeAllHeads
	let chain_subscribe_all_heads_name: &str = "chain_subscribeAllHeads";
	io.add_sync_method(chain_subscribe_all_heads_name, |_: Params| {
		let parsed = "world";
		Ok(Value::String(format!("hello, {}", parsed)))
	});

	// state_getMetadata
	let state_get_metadata_name: &str = "state_getMetadata";
	io.add_sync_method(state_get_metadata_name, |_: Params| {
		let parsed = "world";
		Ok(Value::String(format!("hello, {}", parsed)))
	});

	// state_getRuntimeVersion
	let state_get_runtime_version_name: &str = "state_getRuntimeVersion";
	io.add_sync_method(state_get_runtime_version_name, |_: Params| {
		let parsed = "world";
		Ok(Value::String(format!("hello, {}", parsed)))
	});

	// state_get
	let state_get_name: &str = "state_get";
	io.add_sync_method(state_get_name, |_: Params| {
		let parsed = "world";
		Ok(Value::String(format!("hello, {}", parsed)))
	});

	// system_health
	let state_health_name: &str = "system_health";
	io.add_sync_method(state_health_name, |_: Params| {
		let parsed = "world";
		Ok(Value::String(format!("hello, {}", parsed)))
	});

	// system_name
	let state_name_name: &str = "system_name";
	io.add_sync_method(state_name_name, |_: Params| {
		let parsed = "world";
		Ok(Value::String(format!("hello, {}", parsed)))
	});

	// system_version
	let state_version_name: &str = "system_version";
	io.add_sync_method(state_version_name, |_: Params| {
		let parsed = "world";
		Ok(Value::String(format!("hello, {}", parsed)))
	});

	// returns all rpcs methods
	let rpc_methods_string = get_all_rpc_methods_string(&io);
	io.add_sync_method("rpc_methods", move |_: Params| {
		Ok(Value::String(rpc_methods_string.to_owned()))
	});

	io
}

pub fn side_chain_io_handler<ImportFn, Error>(import_fn: ImportFn) -> IoHandler
where
	ImportFn: Fn(Vec<SignedBlock>) -> Result<(), Error> + Sync + Send + 'static,
	Error: std::fmt::Debug,
{
	let io = IoHandler::new();
	import_block_api::add_import_block_rpc_method(import_fn, io)
}

#[cfg(feature = "test")]
pub mod tests {
	use super::*;
	use std::string::ToString;

	pub fn test_given_io_handler_methods_then_retrieve_all_names_as_string() {
		let mut io = IoHandler::new();
		let method_names: [&str; 4] = ["method1", "another_method", "fancy_thing", "solve_all"];

		for method_name in method_names.iter() {
			io.add_sync_method(method_name, |_: Params| Ok(Value::String("".to_string())));
		}

		let method_string = get_all_rpc_methods_string(&io);

		for method_name in method_names.iter() {
			assert!(method_string.contains(method_name));
		}
	}
}
