use crate::tests::vicut_integration;


#[test]
fn select_in_parens() {
	vicut_integration(
		"This text is (selected) and this is not.",
		&[
		  "-c", "vi)",
		],
		"selected",
	);
}
