use std::marker::PhantomData;

use crate::{mvlookup::LookupTableID, MVLookup};
use ark_ff::PrimeField;
use strum_macros::EnumIter;

pub mod column;
pub mod constraints;
pub mod interpreter;
pub mod witness;

/// The number of intermediate limbs of 4 bits required for the circuit
pub const N_INTERMEDIATE_LIMBS: usize = 20;

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Ord, PartialOrd, EnumIter)]
pub enum LookupTable<Ff> {
    RangeCheck15,
    RangeCheck4,
    RangeCheck4Abs,
    RangeCheckFfHighest(PhantomData<Ff>),
}

impl<Ff: PrimeField> LookupTableID for LookupTable<Ff> {
    fn to_u32(&self) -> u32 {
        match self {
            Self::RangeCheck15 => 1,
            Self::RangeCheck4 => 2,
            Self::RangeCheck4Abs => 3,
            Self::RangeCheckFfHighest(_) => 4,
        }
    }

    fn from_u32(value: u32) -> Self {
        match value {
            1 => Self::RangeCheck15,
            2 => Self::RangeCheck4,
            3 => Self::RangeCheck4Abs,
            4 => Self::RangeCheckFfHighest(PhantomData),
            _ => panic!("Invalid lookup table id"),
        }
    }

    /// All tables are fixed tables.
    fn is_fixed(&self) -> bool {
        true
    }

    fn length(&self) -> usize {
        match self {
            Self::RangeCheck15 => 1 << 15,
            Self::RangeCheck4 => 1 << 4,
            Self::RangeCheck4Abs => 1 << 5,
            Self::RangeCheckFfHighest(_) => TryFrom::try_from(
                crate::serialization::interpreter::ff_modulus_highest_limb::<Ff>(),
            )
            .unwrap(),
        }
    }
}

impl<Ff: PrimeField> LookupTable<Ff> {}

pub type Lookup<F, Ff> = MVLookup<F, LookupTable<Ff>>;

#[cfg(test)]
mod tests {
    use ark_ff::PrimeField;
    use kimchi::circuits::domains::EvaluationDomains;
    use o1_utils::FieldHelpers;
    use poly_commitment::pairing_proof::PairingSRS;
    use rand::Rng as _;
    use std::collections::BTreeMap;

    use super::{Lookup, LookupTable};

    use crate::{
        columns::Column,
        mvlookup::MVLookupWitness,
        precomputed_srs::get_bn254_srs,
        proof::ProofInputs,
        prover::prove,
        serialization::{
            column::SER_N_COLUMNS,
            constraints,
            interpreter::{deserialize_field_element, ff_modulus_highest_limb},
            witness, N_INTERMEDIATE_LIMBS,
        },
        verifier::verify,
        witness::Witness,
        BaseSponge, Ff1, Fp, OpeningProof, ScalarSponge, BN254, N_LIMBS,
    };

    impl<Ff: PrimeField> LookupTable<Ff> {
        fn entries_ff_highest<F: PrimeField>(domain: EvaluationDomains<F>) -> Vec<F> {
            let top_modulus_f = F::from_biguint(&ff_modulus_highest_limb::<Ff>()).unwrap();
            (0..domain.d1.size)
                .map(|i| {
                    if F::from(i) < top_modulus_f {
                        F::from(i)
                    } else {
                        F::zero()
                    }
                })
                .collect()
        }

        fn entries<F: PrimeField>(&self, domain: EvaluationDomains<F>) -> Vec<F> {
            assert!(domain.d1.size >= (1 << 15));
            match self {
                Self::RangeCheck15 => (0..domain.d1.size).map(|i| F::from(i)).collect(),
                Self::RangeCheck4 => (0..domain.d1.size)
                    .map(|i| if i < (1 << 4) { F::from(i) } else { F::zero() })
                    .collect(),
                Self::RangeCheck4Abs => (0..domain.d1.size)
                    .map(|i| {
                        if i < (1 << 4) {
                            // [0,1,2 ... (1<<4)-1]
                            F::from(i)
                        } else if i < 2 * (i << 4) {
                            // [-(i<<4),...-2,-1]
                            F::from(i - 2 * (1 << 4))
                        } else {
                            F::zero()
                        }
                    })
                    .collect(),
                Self::RangeCheckFfHighest(_) => Self::entries_ff_highest::<F>(domain),
            }
        }
    }

    #[test]
    fn test_completeness() {
        let mut rng = o1_utils::tests::make_test_rng();
        // Must be at least 1 << 15 to support rangecheck15
        const DOMAIN_SIZE: usize = 1 << 15;

        let domain = EvaluationDomains::<Fp>::create(DOMAIN_SIZE).unwrap();

        let srs: PairingSRS<BN254> = get_bn254_srs(domain);

        let mut witness_env = witness::Env::<Fp, Ff1>::create();
        // Boxing to avoid stack overflow
        let mut witness: Box<Witness<SER_N_COLUMNS, Vec<Fp>>> = Box::new(Witness {
            cols: Box::new(std::array::from_fn(|_| Vec::with_capacity(DOMAIN_SIZE))),
        });

        // Boxing to avoid stack overflow
        let mut field_elements = vec![];
        // FIXME: we do use always the same values here, because we have a
        // constant check (X - c), different for each row. And there is no
        // constant support/public input yet in the quotient polynomial.
        let (x, y, z) = (
            rng.gen_range(0..1000000),
            rng.gen_range(0..1000000),
            rng.gen_range(0..1000000),
        );
        for _ in 0..DOMAIN_SIZE {
            field_elements.push([x, y, z])
        }

        // An extra element in the array stands for the fixed table.
        let rangecheck4: [Vec<Lookup<Fp, Ff1>>; N_INTERMEDIATE_LIMBS + 1] =
            std::array::from_fn(|_| vec![]);
        let rangecheck4abs: [Vec<Lookup<Fp, Ff1>>; 6 + 1] = std::array::from_fn(|_| vec![]);
        let rangecheck15: [Vec<Lookup<Fp, Ff1>>; N_LIMBS + 1] = std::array::from_fn(|_| vec![]);
        let mut rangecheck_tables: BTreeMap<LookupTable<Ff1>, Vec<Vec<Lookup<Fp, Ff1>>>> =
            BTreeMap::new();
        rangecheck_tables.insert(LookupTable::RangeCheck4, rangecheck4.to_vec());
        rangecheck_tables.insert(LookupTable::RangeCheck4Abs, rangecheck4abs.to_vec());
        rangecheck_tables.insert(LookupTable::RangeCheck15, rangecheck15.to_vec());

        for (_i, limbs) in field_elements.iter().enumerate() {
            // Witness
            deserialize_field_element(&mut witness_env, *limbs);
            // Filling actually used rows
            for j in 0..SER_N_COLUMNS {
                witness.cols[j].push(witness_env.witness.cols[j]);
            }

            for (table_id, table) in rangecheck_tables.iter_mut() {
                for (j, lookup) in witness_env
                    .lookups
                    .get(table_id)
                    .unwrap()
                    .iter()
                    .enumerate()
                {
                    table[j].push(lookup.clone())
                }
            }

            witness_env.reset()
        }

        let constraints = {
            let mut constraints_env = constraints::Env::<Fp, Ff1>::create();
            deserialize_field_element(&mut constraints_env, field_elements[0]);
            constraints_env.get_constraints()
        };

        let mut rangecheck_multiplicities: BTreeMap<LookupTable<Ff1>, Vec<Fp>> = BTreeMap::new();
        for (table_id, table) in rangecheck_tables.iter_mut() {
            let rangecheck_m = witness_env.get_rangecheck15_multiplicities(domain);
            rangecheck_multiplicities.insert(*table_id, rangecheck_m.clone());
            let rangecheck_t = LookupTable::<Ff1>::RangeCheck15
                .entries(domain)
                .into_iter()
                .enumerate()
                .map(|(i, v)| Lookup {
                    table_id: *table_id,
                    numerator: -rangecheck_m[i],
                    value: vec![v],
                });
            *(table.last_mut().unwrap()) = rangecheck_t.collect();
        }

        let mvlookups: Vec<MVLookupWitness<Fp, LookupTable<Ff1>>> = rangecheck_tables
            .iter()
            .map(|(table_id, table)| MVLookupWitness {
                f: table.clone(),
                m: rangecheck_multiplicities[table_id].clone(),
            })
            .collect();

        let proof_inputs = ProofInputs {
            evaluations: *witness,
            mvlookups,
        };

        let proof = prove::<
            _,
            OpeningProof,
            BaseSponge,
            ScalarSponge,
            Column,
            _,
            SER_N_COLUMNS,
            LookupTable<Ff1>,
        >(domain, &srs, &constraints, proof_inputs, &mut rng)
        .unwrap();

        let verifies = verify::<
            _,
            OpeningProof,
            BaseSponge,
            ScalarSponge,
            SER_N_COLUMNS,
            0,
            LookupTable<Ff1>,
        >(
            domain,
            &srs,
            &constraints,
            &proof,
            Witness::zero_vec(DOMAIN_SIZE),
        );
        assert!(verifies)
    }
}
