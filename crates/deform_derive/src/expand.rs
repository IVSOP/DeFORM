use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_quote, Data, DeriveInput, Fields, Index, Result};

use crate::parse;

/// The generated code only wires a struct's selected children together.
/// Interpolation, map membership, and frame scaling live in deform_core.
pub fn expand(input: &DeriveInput) -> Result<TokenStream> {
    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            _ => {
                return Err(syn::Error::new_spanned(
                    input,
                    "Smooth requires a struct with named fields",
                ))
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "Smooth requires a struct with named fields",
            ))
        }
    };
    let overrides = parse::overrides(&input.attrs)?;
    let mut selected = Vec::new();
    for field in fields {
        if parse::selected(field)? {
            selected.push(field);
        }
    }

    let name = &input.ident;
    let smoother = format_ident!("{}Smoother", name);
    let vis = &input.vis;
    let names: Vec<_> = selected.iter().map(|field| &field.ident).collect();
    let types: Vec<_> = selected.iter().map(|field| &field.ty).collect();
    let indices: Vec<_> = (0..selected.len()).map(Index::from).collect();
    let mut generics = input.generics.clone();
    for ty in &types {
        generics
            .make_where_clause()
            .predicates
            .push(parse_quote!(#ty: ::deform_core::Smoothable));
    }
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    Ok(quote! {
        #vis struct #smoother #generics #where_clause {
            // A tuple avoids collisions with the user's field names.
            __fields: (#(<#types as ::deform_core::Smoothable>::Smoother,)*),
            __marker: ::core::marker::PhantomData<fn() -> #name #ty_generics>,
        }

        impl #impl_generics ::core::clone::Clone for #smoother #ty_generics #where_clause {
            fn clone(&self) -> Self {
                Self {
                    __fields: (#(self.__fields.#indices.clone(),)*),
                    __marker: ::core::marker::PhantomData,
                }
            }
        }

        impl #impl_generics ::core::default::Default for #smoother #ty_generics #where_clause {
            fn default() -> Self {
                let mut smoother = Self {
                    __fields: (#(<#types as ::deform_core::Smoothable>::Smoother::default(),)*),
                    __marker: ::core::marker::PhantomData,
                };
                ::deform_core::Smooth::set_params(&mut smoother, ::deform_core::SmoothParams::default());
                smoother
            }
        }

        impl #impl_generics ::deform_core::Smooth<#name #ty_generics> for #smoother #ty_generics #where_clause {
            fn reset(&mut self) {
                #(::deform_core::Smooth::reset(&mut self.__fields.#indices);)*
            }

            fn on_rollback(&mut self, pre: &#name #ty_generics, post: &#name #ty_generics) {
                #(::deform_core::Smooth::on_rollback(&mut self.__fields.#indices, &pre.#names, &post.#names);)*
            }

            fn apply(&mut self, prev: &#name #ty_generics, current: &mut #name #ty_generics, t: f32) {
                #(::deform_core::Smooth::apply(&mut self.__fields.#indices, &prev.#names, &mut current.#names, t);)*
            }

            fn scale_decay(&mut self, ratio: f32) {
                assert!(ratio.is_finite() && ratio >= 0.0, "frame ratio must be finite and nonnegative");
                #(::deform_core::Smooth::scale_decay(&mut self.__fields.#indices, ratio);)*
            }

            fn set_params(&mut self, params: ::deform_core::SmoothParams) {
                params.validate();
                let params = ::deform_core::SmoothParams { #(#overrides,)* ..params };
                #(::deform_core::Smooth::set_params(&mut self.__fields.#indices, params);)*
            }

            fn correction_magnitude_sq(&self) -> f32 {
                0.0 #(+ ::deform_core::Smooth::correction_magnitude_sq(&self.__fields.#indices))*
            }

            fn corrections_discarded(&self) -> u64 {
                0 #(+ ::deform_core::Smooth::corrections_discarded(&self.__fields.#indices))*
            }
        }

        impl #impl_generics ::deform_core::Smoothable for #name #ty_generics #where_clause {
            type Smoother = #smoother #ty_generics;
        }
    })
}
