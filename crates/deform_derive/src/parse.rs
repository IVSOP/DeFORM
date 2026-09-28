use std::collections::HashSet;

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Attribute, Field, Lit, Meta, Result};

/// Authored overrides only: defaults belong to the runtime, never the macro.
pub fn overrides(attrs: &[Attribute]) -> Result<Vec<TokenStream>> {
    let mut seen = HashSet::new();
    let mut assignments = Vec::new();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("smooth")) {
        if !matches!(&attr.meta, Meta::List(_)) {
            return Err(syn::Error::new_spanned(
                attr,
                "expected #[smooth(parameter = number, ...)] on a struct",
            ));
        }
        attr.parse_nested_meta(|meta| {
            let Some(name) = meta.path.get_ident() else {
                return Err(meta.error("expected a smoothing parameter name"));
            };
            let parameter = name.to_string();
            if !matches!(
                parameter.as_str(),
                "decay" | "max_offset" | "min_offset_sq" | "max_correction" | "motion_ratio"
            ) {
                return Err(meta.error("unknown smoothing parameter"));
            }
            if !seen.insert(parameter.clone()) {
                return Err(meta.error("duplicate smoothing parameter"));
            }
            let literal: Lit = meta.value()?.parse()?;
            let value: f32 = match &literal {
                Lit::Float(value) => value.base10_parse()?,
                Lit::Int(value) => value.base10_parse()?,
                _ => {
                    return Err(syn::Error::new_spanned(
                        literal,
                        "expected a nonnegative number",
                    ))
                }
            };
            if !value.is_finite() || value < 0.0 {
                return Err(syn::Error::new_spanned(
                    literal,
                    "expected a finite nonnegative number",
                ));
            }
            if parameter == "decay" && value > 1.0 {
                return Err(syn::Error::new_spanned(literal, "decay must be in [0, 1]"));
            }
            if parameter == "max_offset" {
                let squared = value * value;
                if !squared.is_finite() {
                    return Err(syn::Error::new_spanned(
                        literal,
                        "max_offset squared must fit in f32",
                    ));
                }
                assignments.push(quote! { max_offset_sq: #squared });
            } else {
                assignments.push(quote! { #name: #value });
            }
            Ok(())
        })?;
    }
    Ok(assignments)
}

pub fn selected(field: &Field) -> Result<bool> {
    let mut attrs = field
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("smooth"));
    let Some(attr) = attrs.next() else {
        return Ok(false);
    };
    if let Some(duplicate) = attrs.next() {
        return Err(syn::Error::new_spanned(
            duplicate,
            "duplicate #[smooth] field attribute",
        ));
    }
    match &attr.meta {
        Meta::Path(_) => Ok(true),
        Meta::List(_) => {
            let mode: syn::Ident = attr.parse_args().map_err(|_| {
                syn::Error::new_spanned(
                    attr,
                    "expected #[smooth], #[smooth(nested)], or #[smooth(map)]",
                )
            })?;
            if mode == "nested" || mode == "map" {
                Ok(true)
            } else {
                Err(syn::Error::new_spanned(
                    mode,
                    "unknown field mode; use #[smooth]",
                ))
            }
        }
        Meta::NameValue(_) => Err(syn::Error::new_spanned(
            attr,
            "expected #[smooth] on a field",
        )),
    }
}

#[cfg(test)]
mod tests {
    use syn::{parse_quote, DeriveInput};

    use crate::expand::expand;

    #[test]
    fn rejects_invalid_declarations() {
        let cases = [
            (
                "#[smooth(deacy = 0.5)] struct S { x: f32 }",
                "unknown smoothing parameter",
            ),
            (
                "#[smooth(decay = true)] struct S { x: f32 }",
                "expected a nonnegative number",
            ),
            (
                "#[smooth(decay = 1.1)] struct S { x: f32 }",
                "decay must be in [0, 1]",
            ),
            (
                "#[smooth(max_offset = 1e30)] struct S { x: f32 }",
                "max_offset squared must fit",
            ),
            (
                "#[smooth(decay = 0.5, decay = 0.6)] struct S { x: f32 }",
                "duplicate smoothing parameter",
            ),
            (
                "#[smooth] struct S { x: f32 }",
                "expected #[smooth(parameter",
            ),
            ("struct S { #[smooth(typo)] x: f32 }", "unknown field mode"),
            (
                "struct S { #[smooth(map, nested)] x: f32 }",
                "expected #[smooth]",
            ),
            (
                "struct S { #[smooth(map = true)] x: f32 }",
                "expected #[smooth]",
            ),
            ("struct S { #[smooth()] x: f32 }", "expected #[smooth]"),
            ("struct S { #[smooth = true] x: f32 }", "expected #[smooth]"),
            (
                "struct S { #[smooth] #[smooth] x: f32 }",
                "duplicate #[smooth]",
            ),
            ("struct S(f32);", "named fields"),
            ("enum S { A }", "named fields"),
        ];
        for (source, expected) in cases {
            let input: DeriveInput = syn::parse_str(source).unwrap();
            let error = expand(&input).unwrap_err().to_string();
            assert!(error.contains(expected), "{source}: {error}");
        }
    }

    #[test]
    fn rejects_negative_and_nonfinite_values() {
        for value in ["-1.0", "1e100"] {
            let input = syn::parse_str(&format!(
                "#[smooth(max_correction = {value})] struct S {{ x: f32 }}"
            ))
            .unwrap();
            assert!(expand(&input).is_err());
        }
    }

    #[test]
    fn accepts_integer_parameters_and_legacy_aliases() {
        let input = parse_quote! {
            #[smooth(decay = 1, max_offset = 10)]
            struct S { #[smooth] a: f32, #[smooth(nested)] b: B, #[smooth(map)] c: C }
        };
        assert!(expand(&input).is_ok());
    }
}
