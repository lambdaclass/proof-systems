//! This module implements a prover specialized to generate a proof where only the public input is
//! considered. All other gates are disabled, as well as zero-knowledge, and the permutation
//! argument is hard-coded as the identity function, to minimize the overhead of proving.
//!
//! Proofs generated by this module are explicitly designed to be compatible with the kimchi
//! verifier, and hence the pickles verifier circuit.

use crate::{
    circuits::{
        argument::ArgumentType,
        constraints::FeatureFlags,
        domains::EvaluationDomains,
        lookup::lookups::{LookupFeatures, LookupPatterns},
        polynomials::permutation,
        wires::{COLUMNS, PERMUTS},
    },
    curve::KimchiCurve,
    error::ProverError,
    plonk_sponge::FrSponge,
    proof::{
        PointEvaluations, ProofEvaluations, ProverCommitments, ProverProof, RecursionChallenge,
    },
    verifier_index::VerifierIndex,
};
use ark_ec::ProjectiveCurve;
use ark_ff::{Field, One, PrimeField, Zero};
use ark_poly::{
    univariate::DensePolynomial, EvaluationDomain, Evaluations, Polynomial,
    Radix2EvaluationDomain as D, UVPolynomial,
};
use mina_poseidon::{sponge::ScalarChallenge, FqSponge};
use o1_utils::ExtendedDensePolynomial as _;
use once_cell::sync::OnceCell;
use poly_commitment::{
    commitment::{absorb_commitment, b_poly_coefficients, BlindedCommitment, PolyComm},
    evaluation_proof::DensePolynomialOrEvaluations,
    srs::{endos, SRS},
};
use std::array;
use std::sync::Arc;

/// The result of a proof creation or verification.
type Result<T> = std::result::Result<T, ProverError>;

/// Creates a hard-coded verifier index that proofs by this proofs generated by
/// `create_recursive_public_input_only` will satisfy.
pub fn verifier_index<G: KimchiCurve>(
    srs: Arc<SRS<G>>,
    domain: EvaluationDomains<G::ScalarField>,
    num_public_inputs: usize,
    num_prev_challenges: usize,
) -> VerifierIndex<G> {
    let shifts = permutation::Shifts::new(&domain.d1);
    let (endo_q, _endo_r) = endos::<G::OtherCurve>();
    // TODO: Create `FeatureFlags::default`, and use it here and elsewhere.
    let feature_flags = FeatureFlags {
        range_check0: false,
        range_check1: false,
        lookup_features: LookupFeatures {
            patterns: LookupPatterns {
                xor: false,
                lookup: false,
                range_check: false,
                foreign_field_mul: false,
            },
            joint_lookup_used: false,
            uses_runtime_tables: false,
        },
        foreign_field_add: false,
        foreign_field_mul: false,
        xor: false,
        rot: false,
    };
    let (linearization, powers_of_alpha) =
        crate::linearization::expr_linearization(Some(&feature_flags), true);

    let make_comm = |comm| PolyComm {
        unshifted: vec![comm],
        shifted: None,
    };
    VerifierIndex {
        domain: domain.d1,
        max_poly_size: srs.g.len(),
        srs: srs.clone().into(),
        public: num_public_inputs,
        prev_challenges: num_prev_challenges,

        sigma_comm: array::from_fn(|i| PolyComm {
            // Encodes the polynomial `f(x) = x * shifts[i]`, with no blinding.
            // This represents the identity permutation.
            unshifted: vec![srs.g[1].mul(shifts.shifts[i]).into_affine()],
            shifted: None,
        }),
        coefficients_comm: array::from_fn(|i| {
            make_comm(if i == 0 {
                // The polynomial `f(x) = 1`, without blinding.
                srs.g[0]
            } else {
                // The polynomial `f(x) = 0`, with blinding factor 1.
                // This blinding allows us to represent the commitment in affine coordinates.
                srs.h
            })
        }),
        // The polynomial `f(x) = 1`, without blinding.
        // The generic gate is enabled on every row; combined with the coefficients, and with the
        // mixin for public inputs, this encodes the equation `witness[0] = public_input`.
        generic_comm: make_comm(srs.g[0]),
        // The polynomials `f(x) = 0`, with blinding factor 1.
        // This disables these gates.
        psm_comm: make_comm(srs.h),
        complete_add_comm: make_comm(srs.h),
        mul_comm: make_comm(srs.h),
        emul_comm: make_comm(srs.h),
        endomul_scalar_comm: make_comm(srs.h),

        // Disable all optional gates explicitly.
        range_check0_comm: None,
        range_check1_comm: None,
        foreign_field_add_comm: None,
        foreign_field_mul_comm: None,
        xor_comm: None,
        rot_comm: None,

        shift: shifts.shifts.clone(),
        zkpm: OnceCell::new(),
        w: OnceCell::new(),
        endo: endo_q,
        lookup_index: None,

        linearization,
        powers_of_alpha,
    }
}

impl<G: KimchiCurve> ProverProof<G>
where
    G::BaseField: PrimeField,
{
    /// Generate a proof where the witness column is identical to the public input, and all other
    /// columns are 0.
    /// Proofs generated by this function are compatible with the kimchi verifier, and hence the
    /// pickles verifier circuit.
    ///
    /// # Errors
    ///
    /// Will give error if `create_recursive_public_input_only` process fails.
    pub fn create_public_input_only<
        EFqSponge: Clone + FqSponge<G::BaseField, G, G::ScalarField>,
        EFrSponge: FrSponge<G::ScalarField>,
    >(
        groupmap: &G::Map,
        witness: Vec<G::ScalarField>,
        index: &VerifierIndex<G>,
    ) -> Result<Self> {
        Self::create_recursive_public_input_only::<EFqSponge, EFrSponge>(
            groupmap,
            witness,
            index,
            Vec::new(),
        )
    }

    /// Generate a proof where the witness column is identical to the public input, and all other
    /// columns are 0, including any recursion challenges provided.
    /// Proofs generated by this function are compatible with the kimchi verifier, and hence the
    /// pickles verifier circuit.
    ///
    /// # Errors
    ///
    /// Will give an error if the witness vector is too large for the domain specified in the
    /// verifier index.
    pub fn create_recursive_public_input_only<
        EFqSponge: Clone + FqSponge<G::BaseField, G, G::ScalarField>,
        EFrSponge: FrSponge<G::ScalarField>,
    >(
        group_map: &G::Map,
        mut witness: Vec<G::ScalarField>,
        index: &VerifierIndex<G>,
        prev_challenges: Vec<RecursionChallenge<G>>,
    ) -> Result<Self> {
        let d1_size = index.domain.size();

        let (_, endo_r) = G::endos();

        let srs = index.srs.get().unwrap();

        // Pad the witness to the full domain size, or raise an error if the witness is too large.
        {
            let length_witness = witness.len();
            let length_padding = d1_size
                .checked_sub(length_witness)
                .ok_or(ProverError::NoRoomForZkInWitness)?;
            witness.extend(std::iter::repeat(G::ScalarField::zero()).take(length_padding));
        }

        let mut fq_sponge = EFqSponge::new(G::OtherCurve::sponge_params());

        // TODO: This could be cached for most of the relevant use-cases.
        let verifier_index_digest = index.digest::<EFqSponge>();
        fq_sponge.absorb_fq(&[verifier_index_digest]);

        for RecursionChallenge { comm, .. } in &prev_challenges {
            absorb_commitment(&mut fq_sponge, comm)
        }

        let (witness_poly, unblinded_witness_comm) = {
            let witness_evals =
                Evaluations::<G::ScalarField, D<G::ScalarField>>::from_vec_and_domain(
                    witness,
                    index.domain,
                );
            // We commit using evaluations, because nearly all will be 0, and so we can skip most of
            // the domain size.
            let unblinded_witness_comm =
                srs.commit_evaluations_non_hiding(index.domain, &witness_evals);
            (witness_evals.interpolate(), unblinded_witness_comm)
        };

        // The goal of this circuit is to represent that `witness[0] = public`, so we can
        // explicitly compute the negated public polynomial from the witness.
        let public_poly = -witness_poly.clone();

        // Create and absorb a blinded commitment to the negated public polynomial. We use blinding factor 1
        // to keep compatibility with the logic in the kimchi verifier.
        {
            let public_comm = unblinded_witness_comm.map(|x| srs.h + x.neg());
            absorb_commitment(&mut fq_sponge, &public_comm);
        }

        let w_comm: [PolyComm<G>; COLUMNS] = {
            let mut w_comm = Vec::with_capacity(COLUMNS);

            // Blind the witness commitment with blinding factor 1, to allow for a zero public input
            // vector.
            w_comm.push(unblinded_witness_comm.map(|x| x + srs.h));

            for _ in 1..COLUMNS {
                w_comm.push(PolyComm {
                    unshifted: vec![srs.h], // `f(x) = 0` with blinding factor 1
                    shifted: None,
                });
            }

            w_comm
                .try_into()
                .expect("previous loop is of the correct length")
        };

        w_comm
            .iter()
            .for_each(|c| absorb_commitment(&mut fq_sponge, &c));

        let beta = fq_sponge.challenge();
        let gamma = fq_sponge.challenge();

        let z_comm = {
            // Due to the identity permutation, we know statically that all of the non-zk rows will
            // evaluate to 1. Since we also don't care about zero-knowledge here, we can use the
            // constant polynomial `f(x) = 1` as a satisfying instance.
            let z_comm = PolyComm {
                unshifted: vec![srs.g[0]], // `f(x) = 1` with no blinding.
                shifted: None,
            };
            absorb_commitment(&mut fq_sponge, &z_comm);
            z_comm
        };

        let alpha = ScalarChallenge(fq_sponge.challenge()).to_field(endo_r);

        let t_comm = {
            // All polynomials in the circuit evaluate to exactly the 0 polynomial.
            // We use this fact to ommit the calculation of the quotient and emit a (blinded) zero
            // commitment directly.
            let t_comm = BlindedCommitment {
                commitment: PolyComm {
                    unshifted: vec![srs.h; 7], // `f(x) = 0`, with blinding factor 1, in 7 chunks.
                    shifted: None,
                },
                blinders: PolyComm {
                    unshifted: vec![G::ScalarField::one(); 7],
                    shifted: None,
                },
            };
            absorb_commitment(&mut fq_sponge, &t_comm.commitment);
            t_comm
        };

        //~ 1. Sample $\zeta'$ with the Fq-Sponge.
        let zeta_chal = ScalarChallenge(fq_sponge.challenge());

        //~ 1. Derive $\zeta$ from $\zeta'$ using the endomorphism (TODO: specify)
        let zeta = zeta_chal.to_field(endo_r);

        let omega = index.domain.group_gen;
        let zeta_omega = zeta * omega;

        let chunked_evals = {
            let constant_evals = |x| PointEvaluations {
                zeta: vec![x],
                zeta_omega: vec![x],
            };

            ProofEvaluations::<PointEvaluations<Vec<G::ScalarField>>> {
                s: array::from_fn(|i| {
                    // Inlined computations of `f(x) = x * shift[i]`.
                    PointEvaluations {
                        zeta: vec![zeta * index.shift[i]],
                        zeta_omega: vec![zeta_omega * index.shift[i]],
                    }
                }),
                coefficients: array::from_fn(|i| {
                    if i == 0 {
                        // The first coefficient column represents `f(x) = 1`.
                        constant_evals(G::ScalarField::one())
                    } else {
                        // The remaining coefficient columns represent `f(x) = 0`.
                        constant_evals(G::ScalarField::zero())
                    }
                }),
                w: array::from_fn(|i| {
                    if i == 0 {
                        // Compute the evaluations for our non-zero witness column.
                        let chunked = witness_poly.to_chunked_polynomial(index.max_poly_size);
                        PointEvaluations {
                            zeta: chunked.evaluate_chunks(zeta),
                            zeta_omega: chunked.evaluate_chunks(zeta_omega),
                        }
                    } else {
                        // The rest of the witness columns are 0, by construction.
                        constant_evals(G::ScalarField::zero())
                    }
                }),

                // As above in `z_comm`, we have selected the polynomial `f(x) = 1` as our
                // satisfying witness, so we can hard-code the evaluation 1 here.
                z: constant_evals(G::ScalarField::one()),

                // Enabled on every row, via `f(x) = 1`.
                generic_selector: constant_evals(G::ScalarField::one()),
                // Disabled everywhere, via `f(x) = 0`.
                poseidon_selector: constant_evals(G::ScalarField::zero()),
                complete_add_selector: constant_evals(G::ScalarField::zero()),
                mul_selector: constant_evals(G::ScalarField::zero()),
                emul_selector: constant_evals(G::ScalarField::zero()),
                endomul_scalar_selector: constant_evals(G::ScalarField::zero()),

                // All optional gates are disabled.
                range_check0_selector: None,
                range_check1_selector: None,
                foreign_field_add_selector: None,
                foreign_field_mul_selector: None,
                xor_selector: None,
                rot_selector: None,
                runtime_lookup_table_selector: None,
                xor_lookup_selector: None,
                lookup_gate_lookup_selector: None,
                range_check_lookup_selector: None,
                foreign_field_mul_lookup_selector: None,

                // The lookup argument is disabled.
                lookup_aggregation: None,
                lookup_table: None,
                lookup_sorted: array::from_fn(|_| None),
                runtime_lookup_table: None,
            }
        };

        let zeta_to_srs_len = zeta.pow([index.max_poly_size as u64]);
        let zeta_omega_to_srs_len = zeta_omega.pow([index.max_poly_size as u64]);
        let zeta_to_domain_size = zeta.pow([d1_size as u64]);

        // TODO: We know statically that all chunks are of size 1, so this is technically unnecessary.
        let evals = {
            let powers_of_eval_points_for_chunks = PointEvaluations {
                zeta: zeta_to_srs_len,
                zeta_omega: zeta_omega_to_srs_len,
            };
            chunked_evals.combine(&powers_of_eval_points_for_chunks)
        };

        // Compute the difference between the linearization polynomial and `(zeta^n - 1) * quotient`.
        let ft: DensePolynomial<G::ScalarField> = {
            // We know statically that the quotient polynomial is 0, and the only linearized part
            // of the proof is the permutation argument, so we compute that part of the
            // linearization explicitly here.
            let mut all_alphas = index.powers_of_alpha.clone();
            all_alphas.instantiate(alpha);
            let alphas = all_alphas.get_alphas(ArgumentType::Permutation, permutation::CONSTRAINTS);
            let scalar =
                crate::circuits::constraints::ConstraintSystem::<G::ScalarField>::perm_scalars(
                    &evals,
                    beta,
                    gamma,
                    alphas,
                    permutation::eval_zk_polynomial(index.domain, zeta),
                );

            // Construct the linearized polynomial `scalar * permutation_coefficients[PERMUS-1]`
            // explicitly. In particular, since we know that
            // `permutation_coefficients[PERMUTS-1](x) = x * shifts[PERMUTS-1]`, we know that the
            // desired polynomial will be `f(x) = x * (scalar * shifts[PERMUTS-1])`.
            DensePolynomial::from_coefficients_vec(vec![
                G::ScalarField::zero(),
                scalar * index.shift[PERMUTS - 1],
            ])
        };

        let blinding_ft = {
            let blinding_t = t_comm.blinders.chunk_blinding(zeta_to_srs_len);
            let blinding_f = G::ScalarField::zero();

            PolyComm {
                // blinding_f - Z_H(zeta) * blinding_t
                unshifted: vec![
                    blinding_f - (zeta_to_domain_size - G::ScalarField::one()) * blinding_t,
                ],
                shifted: None,
            }
        };

        let ft_eval1 = ft.evaluate(&zeta_omega);

        let fq_sponge_before_evaluations = fq_sponge.clone();

        let mut fr_sponge = {
            let mut fr_sponge = EFrSponge::new(G::sponge_params());
            fr_sponge.absorb(&fq_sponge.digest());
            fr_sponge
        };

        {
            let prev_challenge_digest = {
                let mut fr_sponge = EFrSponge::new(G::sponge_params());
                for RecursionChallenge { chals, .. } in &prev_challenges {
                    fr_sponge.absorb_multiple(chals);
                }
                fr_sponge.digest()
            };
            fr_sponge.absorb(&prev_challenge_digest);
        }

        fr_sponge.absorb(&ft_eval1);

        {
            let public_input_eval_zeta = vec![-evals.w[0].zeta];
            fr_sponge.absorb_multiple(&public_input_eval_zeta);
            let public_input_eval_zeta_omega = vec![-evals.w[0].zeta_omega];
            fr_sponge.absorb_multiple(&public_input_eval_zeta_omega);
        }

        fr_sponge.absorb_evaluations(&chunked_evals);

        let v = fr_sponge.challenge().to_field(endo_r);
        let u = fr_sponge.challenge().to_field(endo_r);

        // `DensePolynomialOrEvaluation` takes a reference, so we have to allocate the polynomials
        // that we will use here to make sure they live long enough.
        let recursion_polynomials = prev_challenges
            .iter()
            .map(|RecursionChallenge { chals, comm }| {
                (
                    DensePolynomial::from_coefficients_vec(b_poly_coefficients(chals)),
                    comm.unshifted.len(),
                )
            })
            .collect::<Vec<_>>();
        let one_polynomial = DensePolynomial::from_coefficients_vec(vec![G::ScalarField::one()]);
        let zero_polynomial = DensePolynomial::from_coefficients_vec(vec![]);
        let shifted_polys: Vec<_> = index
            .shift
            .iter()
            .map(|shift| {
                DensePolynomial::from_coefficients_vec(vec![G::ScalarField::zero(), *shift])
            })
            .collect();

        let polynomials_to_open = {
            // Helpers
            let non_hiding = |d1_size: usize| PolyComm {
                unshifted: vec![G::ScalarField::zero(); d1_size],
                shifted: None,
            };
            let fixed_hiding = |d1_size: usize| PolyComm {
                unshifted: vec![G::ScalarField::one(); d1_size],
                shifted: None,
            };
            let coefficients_form = DensePolynomialOrEvaluations::<_, D<_>>::DensePolynomial;

            let mut polynomials_to_open = recursion_polynomials
                .iter()
                .map(|(p, d1_size)| (coefficients_form(p), None, non_hiding(*d1_size)))
                .collect::<Vec<_>>();
            // public polynomial
            polynomials_to_open.push((coefficients_form(&public_poly), None, fixed_hiding(1)));
            // ft polynomial
            polynomials_to_open.push((coefficients_form(&ft), None, blinding_ft));
            // permutation aggregation polynomial
            polynomials_to_open.push((coefficients_form(&one_polynomial), None, non_hiding(1)));
            // generic selector
            polynomials_to_open.push((coefficients_form(&one_polynomial), None, non_hiding(1)));
            // other selectors
            polynomials_to_open.push((coefficients_form(&zero_polynomial), None, fixed_hiding(1)));
            polynomials_to_open.push((coefficients_form(&zero_polynomial), None, fixed_hiding(1)));
            polynomials_to_open.push((coefficients_form(&zero_polynomial), None, fixed_hiding(1)));
            polynomials_to_open.push((coefficients_form(&zero_polynomial), None, fixed_hiding(1)));
            polynomials_to_open.push((coefficients_form(&zero_polynomial), None, fixed_hiding(1)));
            // witness columns
            polynomials_to_open.push((coefficients_form(&witness_poly), None, fixed_hiding(1)));
            polynomials_to_open.extend(
                (1..COLUMNS).map(|_| (coefficients_form(&zero_polynomial), None, fixed_hiding(1))),
            );
            // coefficients
            polynomials_to_open.push((coefficients_form(&one_polynomial), None, non_hiding(1)));
            polynomials_to_open.extend(
                (1..COLUMNS).map(|_| (coefficients_form(&zero_polynomial), None, fixed_hiding(1))),
            );
            // permutation coefficients
            polynomials_to_open.extend(
                shifted_polys
                    .iter()
                    .take(PERMUTS - 1)
                    .map(|w| (coefficients_form(w), None, non_hiding(1)))
                    .collect::<Vec<_>>(),
            );
            polynomials_to_open
        };

        // TODO: rng should be passed as arg
        let rng = &mut rand::rngs::OsRng;

        let opening_proof = srs.open(
            group_map,
            &polynomials_to_open,
            &[zeta, zeta_omega],
            v,
            u,
            fq_sponge_before_evaluations,
            rng,
        );

        Ok(Self {
            commitments: ProverCommitments {
                w_comm,
                z_comm,
                t_comm: t_comm.commitment,
                lookup: None,
            },
            proof: opening_proof,
            evals: chunked_evals,
            ft_eval1,
            prev_challenges,
        })
    }
}

#[test]
fn test_public_input_only_prover() {
    use crate::{circuits::domains::EvaluationDomains, verifier::verify};
    use groupmap::GroupMap;
    use mina_curves::pasta::{Fq, Pallas, PallasParameters};
    use mina_poseidon::{
        constants::PlonkSpongeConstantsKimchi,
        sponge::{DefaultFqSponge, DefaultFrSponge},
    };
    use poly_commitment::{commitment::CommitmentCurve, srs::SRS};
    use std::{sync::Arc, time::Instant};

    type SpongeParams = PlonkSpongeConstantsKimchi;
    type BaseSponge = DefaultFqSponge<PallasParameters, SpongeParams>;
    type ScalarSponge = DefaultFrSponge<Fq, SpongeParams>;

    let start = Instant::now();

    let circuit_size = (2 << 16) - 1;

    let domain = EvaluationDomains::<Fq>::create(circuit_size).unwrap();

    let mut srs = SRS::<Pallas>::create(domain.d1.size());
    srs.add_lagrange_basis(domain.d1);
    let srs = Arc::new(srs);

    println!("- time to create srs: {:?}ms", start.elapsed().as_millis());

    let start = Instant::now();

    let num_prev_challenges = 0;

    let num_public_inputs = 4;

    let verifier_index =
        verifier_index::<Pallas>(srs, domain, num_public_inputs, num_prev_challenges);
    println!(
        "- time to create verifier index: {:?}ms",
        start.elapsed().as_millis()
    );

    let public_inputs = vec![
        Fq::from(5u64),
        Fq::from(10u64),
        Fq::from(15u64),
        Fq::from(20u64),
    ];

    let start = Instant::now();

    let group_map = <Pallas as CommitmentCurve>::Map::setup();

    let proof = ProverProof::create_recursive_public_input_only::<BaseSponge, ScalarSponge>(
        &group_map,
        public_inputs.clone(),
        &verifier_index,
        vec![],
    )
    .unwrap();
    println!(
        "- time to create proof: {:?}ms",
        start.elapsed().as_millis()
    );

    let start = Instant::now();
    verify::<Pallas, BaseSponge, ScalarSponge>(&group_map, &verifier_index, &proof, &public_inputs)
        .unwrap();
    println!("- time to verify: {}ms", start.elapsed().as_millis());
}
