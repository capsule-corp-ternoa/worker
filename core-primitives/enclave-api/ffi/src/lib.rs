///! FFI's that call into the enclave. These functions need to be added to the
/// enclave edl file and be implemented within the enclave.
use sgx_types::{c_int, sgx_enclave_id_t, sgx_quote_sign_type_t, sgx_status_t};

extern "C" {

	pub fn init(
		eid: sgx_enclave_id_t,
		retval: *mut sgx_status_t,
		mu_ra_addr: *const u8,
		mu_ra_addr_size: u32,
		untrusted_worker_addr: *const u8,
		untrusted_worker_addr_size: u32,
	) -> sgx_status_t;

	pub fn init_direct_invocation_server(
		eid: sgx_enclave_id_t,
		retval: *mut sgx_status_t,
		server_addr: *const u8,
		server_addr_size: u32,
	) -> sgx_status_t;

	pub fn init_light_client(
		eid: sgx_enclave_id_t,
		retval: *mut sgx_status_t,
		genesis_hash: *const u8,
		genesis_hash_size: usize,
		authority_list: *const u8,
		authority_list_size: usize,
		authority_proof: *const u8,
		authority_proof_size: usize,
		latest_header: *mut u8,
		latest_header_size: usize,
	) -> sgx_status_t;

	pub fn sync_parentchain(
		eid: sgx_enclave_id_t,
		retval: *mut sgx_status_t,
		blocks: *const u8,
		blocks_size: usize,
		nonce: *const u32,
	) -> sgx_status_t;

	pub fn set_nonce(
		eid: sgx_enclave_id_t,
		retval: *mut sgx_status_t,
		nonce: *const u32,
	) -> sgx_status_t;

	pub fn get_rsa_encryption_pubkey(
		eid: sgx_enclave_id_t,
		retval: *mut sgx_status_t,
		pubkey: *mut u8,
		pubkey_size: u32,
	) -> sgx_status_t;

	pub fn get_ecc_signing_pubkey(
		eid: sgx_enclave_id_t,
		retval: *mut sgx_status_t,
		pubkey: *mut u8,
		pubkey_size: u32,
	) -> sgx_status_t;

	pub fn get_mrenclave(
		eid: sgx_enclave_id_t,
		retval: *mut sgx_status_t,
		mrenclave: *mut u8,
		mrenclave_size: u32,
	) -> sgx_status_t;

	pub fn perform_ra(
		eid: sgx_enclave_id_t,
		retval: *mut sgx_status_t,
		genesis_hash: *const u8,
		genesis_hash_size: u32,
		nonce: *const u32,
		w_url: *const u8,
		w_url_size: u32,
		unchecked_extrinsic: *mut u8,
		unchecked_extrinsic_size: u32,
	) -> sgx_status_t;

	pub fn dump_ra_to_disk(eid: sgx_enclave_id_t, retval: *mut sgx_status_t) -> sgx_status_t;

	pub fn test_main_entrance(eid: sgx_enclave_id_t, retval: *mut sgx_status_t) -> sgx_status_t;

	pub fn mock_register_enclave_xt(
		eid: sgx_enclave_id_t,
		retval: *mut sgx_status_t,
		genesis_hash: *const u8,
		genesis_hash_size: u32,
		nonce: *const u32,
		w_url: *const u8,
		w_url_size: u32,
		unchecked_extrinsic: *mut u8,
		unchecked_extrinsic_size: u32,
	) -> sgx_status_t;

	pub fn run_key_provisioning_server(
		eid: sgx_enclave_id_t,
		retval: *mut sgx_status_t,
		socket_fd: c_int,
		sign_type: sgx_quote_sign_type_t,
		skip_ra: c_int,
	) -> sgx_status_t;

	pub fn request_key_provisioning(
		eid: sgx_enclave_id_t,
		retval: *mut sgx_status_t,
		socket_fd: c_int,
		sign_type: sgx_quote_sign_type_t,
		skip_ra: c_int,
	) -> sgx_status_t;
}
