use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, Data, DeriveInput, Fields, Lit};

/// Derives a companion smoother struct and implements `Smooth<T>` for it.
///
/// # Usage
///
/// ```ignore
/// #[derive(Smooth)]
/// #[smooth(decay = 0.9, max_offset = 200.0, min_offset_sq = 4.0)]
/// struct GameState {
///     #[smooth]
///     player_pos: Vec2,
///     #[smooth]
///     ball_pos: Vec2,
///     score: u32,  // not smoothed
/// }
/// ```
///
/// Generates `GameStateSmoother` implementing `Smooth<GameState>`.
/// Fields marked `#[smooth]` must implement `SmoothableField` and support `-`, `+=`, `*= f32`.
#[proc_macro_derive(Smooth, attributes(smooth))]
pub fn derive_smooth(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let name = &input.ident;
    let smoother_name = format_ident!("{}Smoother", name);
    let vis = &input.vis;

    let mut decay: f32 = 0.9;
    let mut max_offset: f32 = 200.0;
    let mut min_offset_sq: f32 = 4.0;

    for attr in &input.attrs {
        if attr.path().is_ident("smooth") {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("decay") {
                    let value = meta.value()?;
                    let lit: Lit = value.parse()?;
                    if let Lit::Float(f) = &lit {
                        decay = f.base10_parse()?;
                    } else if let Lit::Int(i) = &lit {
                        decay = i.base10_parse()?;
                    }
                } else if meta.path.is_ident("max_offset") {
                    let value = meta.value()?;
                    let lit: Lit = value.parse()?;
                    if let Lit::Float(f) = &lit {
                        max_offset = f.base10_parse()?;
                    } else if let Lit::Int(i) = &lit {
                        max_offset = i.base10_parse()?;
                    }
                } else if meta.path.is_ident("min_offset_sq") {
                    let value = meta.value()?;
                    let lit: Lit = value.parse()?;
                    if let Lit::Float(f) = &lit {
                        min_offset_sq = f.base10_parse()?;
                    } else if let Lit::Int(i) = &lit {
                        min_offset_sq = i.base10_parse()?;
                    }
                }
                Ok(())
            })
            .unwrap_or_else(|e| panic!("failed to parse #[smooth(...)]: {e}"));
        }
    }

    let max_offset_sq = max_offset * max_offset;

    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            _ => panic!("Smooth can only be derived for structs with named fields"),
        },
        _ => panic!("Smooth can only be derived for structs"),
    };

    let smooth_fields: Vec<_> = fields
        .iter()
        .filter(|f| f.attrs.iter().any(|a| a.path().is_ident("smooth")))
        .collect();

    let smoother_field_defs = smooth_fields.iter().map(|f| {
        let name = &f.ident;
        let ty = &f.ty;
        quote! { pub #name: #ty }
    });

    let reset_stmts = smooth_fields.iter().map(|f| {
        let name = &f.ident;
        quote! { self.#name = Default::default(); }
    });

    let rollback_stmts = smooth_fields.iter().map(|f| {
        let name = &f.ident;
        quote! {
            {
                let mut pre_visual = old.#name.clone();
                pre_visual += self.#name.clone();
                self.#name = pre_visual - new.#name.clone();
                if ::deform_core::SmoothableField::magnitude_sq(&self.#name) > #max_offset_sq {
                    self.#name = Default::default();
                }
            }
        }
    });

    let apply_stmts = smooth_fields.iter().map(|f| {
        let name = &f.ident;
        quote! {
            self.#name *= #decay;
            if ::deform_core::SmoothableField::magnitude_sq(&self.#name) < #min_offset_sq {
                self.#name = Default::default();
            }
            state.#name += self.#name.clone();
        }
    });

    let expanded = quote! {
        #[derive(Default, Clone)]
        #vis struct #smoother_name {
            #(#smoother_field_defs,)*
        }

        impl ::deform_core::Smooth<#name> for #smoother_name {
            fn reset(&mut self) {
                #(#reset_stmts)*
            }

            fn on_rollback(&mut self, old: &#name, new: &#name) {
                #(#rollback_stmts)*
            }

            fn apply(&mut self, state: &mut #name) {
                #(#apply_stmts)*
            }
        }
    };

    TokenStream::from(expanded)
}
