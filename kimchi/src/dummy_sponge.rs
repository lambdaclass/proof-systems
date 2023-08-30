use ark_ec::short_weierstrass_jacobian::GroupAffine;
use mina_curves::pasta::{Fp, Fq, VestaParameters};
use mina_poseidon::{poseidon::ArithmeticSpongeParams, sponge::ScalarChallenge, FqSponge};

use crate::{
    plonk_sponge::FrSponge,
    proof::{PointEvaluations, ProofEvaluations},
};

#[derive(Clone)]
pub struct DummyFqSponge {}

#[derive(Clone)]
pub struct DummyFrSponge {}

impl FqSponge<Fq, GroupAffine<VestaParameters>, Fp> for DummyFqSponge {
    fn new(_p: &'static ArithmeticSpongeParams<Fq>) -> Self {
        Self {}
    }

    fn absorb_g(&mut self, _g: &[GroupAffine<VestaParameters>]) {}

    fn absorb_fq(&mut self, _x: &[Fq]) {}

    fn absorb_fr(&mut self, _x: &[Fp]) {}

    fn challenge(&mut self) -> Fp {
        Fp::from(1)
    }

    fn challenge_fq(&mut self) -> Fq {
        Fq::from(1)
    }

    fn digest(self) -> Fp {
        Fp::from(1)
    }

    fn digest_fq(self) -> Fq {
        Fq::from(1)
    }
}

impl FrSponge<Fp> for DummyFrSponge {
    fn new(_p: &'static ArithmeticSpongeParams<Fp>) -> Self {
        Self {}
    }

    fn absorb(&mut self, _x: &Fp) {}

    fn absorb_multiple(&mut self, _x: &[Fp]) {}

    fn challenge(&mut self) -> ScalarChallenge<Fp> {
        ScalarChallenge(Fp::from(1))
    }

    fn digest(self) -> Fp {
        Fp::from(1)
    }

    fn absorb_evaluations(&mut self, _e: &ProofEvaluations<PointEvaluations<Vec<Fp>>>) {}
}
