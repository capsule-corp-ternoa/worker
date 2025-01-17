//! Abstraction over the state crypto that is used in the enclave
use std::{fmt::Debug, vec::Vec};

pub trait StateCrypto {
	type Error: Debug;
	fn encrypt(&self, data: &mut [u8]) -> Result<(), Self::Error>;
	fn decrypt(&self, data: &mut [u8]) -> Result<(), Self::Error>;
}

pub trait ShieldingCrypto {
	type Error: Debug;
	fn encrypt(&self, data: &[u8]) -> Result<Vec<u8>, Self::Error>;
	fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, Self::Error>;
}
