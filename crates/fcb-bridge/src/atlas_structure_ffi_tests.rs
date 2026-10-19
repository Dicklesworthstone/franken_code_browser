#![deny(unsafe_op_in_unsafe_fn)]
use super::*;

#[test]
fn structural_query_signature_and_public_header_match() {
    let _: unsafe extern "C" fn(u64, u64, u64, u64, *const c_char, *const c_char,
        *const c_char, u64, *const c_char, u64, u64, u64, u64) -> *mut c_char = fcb_atlas_structure_begin;
    let header = include_str!("../include/fcb_atlas_structure.h");
    assert_eq!(header.matches("fcb_atlas_structure_begin(").count(), 1);
    assert!(header.contains("fcb_atlas_search_step"));
    assert!(header.contains("semantic_complete"));
}
