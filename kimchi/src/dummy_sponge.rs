use mina_poseidon::FqSponge;

pub struct DummySponge {}

impl FqSponge for DummySponge {
    fn new(p: &'static mina_poseidon::poseidon::ArithmeticSpongeParams<Fq>) -> Self {
        Self {}
    }

    fn absorb_g(&mut self, g: &[G]) {}

    fn absorb_fq(&mut self, x: &[Fq]) {}

    fn absorb_fr(&mut self, x: &[Fr]) {}

    fn challenge(&mut self) -> Fr {}

    fn challenge_fq(&mut self) -> Fq {
        todo!()
    }

    fn digest(self) -> Fr {
        todo!()
    }

    fn digest_fq(self) -> Fq {
        todo!()
    }
}
