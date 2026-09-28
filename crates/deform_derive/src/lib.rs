//! Derive support for `deform_core`'s recursive smoothing API.

use proc_macro::TokenStream;
use syn::{parse_macro_input, DeriveInput};

mod expand;
mod parse;

/// Generates `TypeSmoother` and implements `Smoothable` for a named-field struct.
///
/// Select fields with `#[smooth]`: numbers, vectors, derived structs, and maps all
/// use the same recursive interface. Unmarked fields are untouched. The legacy
/// spellings `#[smooth(nested)]` and `#[smooth(map)]` are equivalent aliases.
///
/// On a struct, `#[smooth(decay = 0.8, max_offset = 10.0)]` overrides just those
/// parameters; all others inherit from the parent (or the root defaults).
/// Parameters are numeric literals in simulation-tick units:
///
/// | Parameter | Root default | Meaning |
/// |---|---|---|
/// | `decay` | `0.9` | Offset fraction retained per tick, in `[0, 1]` |
/// | `max_offset` | `200.0` | Distance above which corrections/movements snap |
/// | `min_offset_sq` | `4.0` | Squared distance below which residual offsets clear |
/// | `max_correction` | disabled | Extra distance removed per tick after decay |
/// | `motion_ratio` | disabled | Offset cap as a multiple of movement per tick |
///
/// See `deform_core::smooth` for a complete example and the runtime API.
#[proc_macro_derive(Smooth, attributes(smooth))]
pub fn derive_smooth(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand::expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
