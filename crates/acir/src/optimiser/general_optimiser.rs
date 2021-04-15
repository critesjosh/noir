use std::{collections::BTreeMap, marker::PhantomData};

use crate::native_types::{Arithmetic, Witness};
use noir_field::FieldElement;

pub struct GeneralOpt<F: FieldElement> {
    phantom: PhantomData<F>,
}

impl<F: FieldElement> GeneralOpt<F> {
    pub fn optimise(gate: Arithmetic<F>) -> Arithmetic<F> {
        // XXX: Perhaps this optimisation can be done on the fly
        let gate = remove_zero_coefficients(gate);
        simplify_mul_terms(gate)
    }
}

// Remove all terms with zero as a coefficient
pub fn remove_zero_coefficients<F: FieldElement>(mut gate: Arithmetic<F>) -> Arithmetic<F> {
    // Check the mul terms
    gate.mul_terms = gate
        .mul_terms
        .into_iter()
        .filter(|(scale, _, _)| !scale.is_zero())
        .collect();

    // Check the lin combination terms
    gate.linear_combinations = gate
        .linear_combinations
        .into_iter()
        .filter(|(scale, _)| !scale.is_zero())
        .collect();

    gate
}

// Simplifies all mul terms with the same bi-variate variables
pub fn simplify_mul_terms<F: FieldElement>(mut gate: Arithmetic<F>) -> Arithmetic<F> {
    let mut hash_map: BTreeMap<(Witness, Witness), F> = BTreeMap::new();

    // Canonicalise the ordering of the multiplication, lets just order by variable name
    for (scale, w_l, w_r) in gate.mul_terms.clone().into_iter() {
        let mut pair = vec![w_l, w_r];
        // Sort using rust sort algorithm
        pair.sort();

        *hash_map.entry((pair[0], pair[1])).or_insert_with(F::zero) += scale;
    }

    gate.mul_terms = hash_map
        .into_iter()
        .map(|((w_l, w_r), scale)| (scale, w_l, w_r))
        .collect();

    gate
}