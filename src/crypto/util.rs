use blake2b_simd::Params;
use pasta_curves::{
    arithmetic::{CurveExt, FieldExt},
    group::ff::PrimeField,
    pallas,
};

use super::constants::fixed_bases::{
    VALUE_COMMITMENT_PERSONALIZATION, VALUE_COMMITMENT_R_BYTES, VALUE_COMMITMENT_V_BYTES,
};
use crate::crypto::{constants::util::gen_const_array, types::*};

pub fn hash_to_scalar(persona: &[u8], a: &[u8], b: &[u8]) -> pallas::Scalar {
    let mut hasher = Params::new().hash_length(64).personal(persona).to_state();
    hasher.update(a);
    hasher.update(b);
    let ret = hasher.finalize();
    pallas::Scalar::from_bytes_wide(ret.as_array())
}

#[allow(non_snake_case)]
pub fn pedersen_commitment_scalar(value: pallas::Scalar, blind: DrkValueBlind) -> DrkValueCommit {
    let hasher = DrkValueCommit::hash_to_curve(VALUE_COMMITMENT_PERSONALIZATION);
    let V = hasher(&VALUE_COMMITMENT_V_BYTES);
    let R = hasher(&VALUE_COMMITMENT_R_BYTES);

    V * value + R * blind
}

pub fn pedersen_commitment_u64(value: u64, blind: DrkValueBlind) -> DrkValueCommit {
    pedersen_commitment_scalar(mod_r_p(DrkValue::from(value)), blind)
}

/// Converts from pallas::Base to pallas::Scalar (aka $x \pmod{r_\mathbb{P}}$).
///
/// This requires no modular reduction because Pallas' base field is smaller than its
/// scalar field.
pub fn mod_r_p(x: pallas::Base) -> pallas::Scalar {
    pallas::Scalar::from_repr(x.to_repr()).unwrap()
}

/// The sequence of bits representing a u64 in little-endian order.
///
/// # Panics
///
/// Panics if the expected length of the sequence `NUM_BITS` exceeds
/// 64.
pub fn i2lebsp<const NUM_BITS: usize>(int: u64) -> [bool; NUM_BITS] {
    assert!(NUM_BITS <= 64);
    gen_const_array(|mask: usize| (int & (1 << mask)) != 0)
}
