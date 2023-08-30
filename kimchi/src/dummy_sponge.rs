use ark_ec::short_weierstrass_jacobian::GroupAffine;
use mina_curves::pasta::{Fp, Fq, VestaParameters};
use mina_poseidon::{FqSponge, poseidon::ArithmeticSpongeParams};

pub struct DummySponge {}

impl FqSponge<Fq, GroupAffine<VestaParameters>, Fp> for DummySponge {
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
