use crate::curve::KimchiCurve;
use crate::mina_poseidon::FqSponge;

pub fn hash_public_input<
    G: KimchiCurve,
    EFqSponge: Clone + FqSponge<G::BaseField, G, G::ScalarField>,
>(
    public_input: &[G::ScalarField],
) -> G::ScalarField {
    let mut tmp_sponge = EFqSponge::new(G::other_curve_sponge_params());
    tmp_sponge.absorb_fr(&public_input);

    tmp_sponge.digest()
}
