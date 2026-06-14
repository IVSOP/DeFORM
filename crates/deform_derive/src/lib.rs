use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{
    parse_macro_input, Data, DeriveInput, Fields, GenericArgument, Lit, PathArguments, Type,
};

/// Derives a companion smoother struct and implements [`Smooth<T>`] for it.
///
/// For a struct named `Foo`, this generates:
/// - `FooSmoother` — a struct holding per-field smoothing offsets
/// - `impl Smooth<Foo> for FooSmoother`
/// - `impl Smoothable for Foo` — links `Foo` to its smoother for composition
///
/// # Struct-level parameters
///
/// Configure smoothing behavior with `#[smooth(...)]` on the struct:
///
/// ```ignore
/// #[derive(Smooth)]
/// #[smooth(decay = 0.85, max_offset = 150.0, min_offset_sq = 1.0)]
/// struct GameState { /* ... */ }
/// ```
///
/// | Parameter      | Default | Description                                                   |
/// |----------------|---------|---------------------------------------------------------------|
/// | `decay`        | `0.9`   | Multiplier applied to offsets each frame (lower = faster snap)|
/// | `max_offset`   | `200.0` | Offsets larger than this are discarded (teleport threshold)   |
/// | `min_offset_sq`| `4.0`   | Offsets with squared magnitude below this are zeroed out      |
///
/// # Field attributes
///
/// ## `#[smooth]` — direct field interpolation + offset decay
///
/// Marks a field for lerp-based interpolation between frames, with offset
/// decay to absorb rollback corrections. The field type must implement
/// [`SmoothableField`] and support `-`, `+=`, and `*= f32`.
///
/// ## `#[smooth(nested)]` — delegate to a child smoother
///
/// Delegates smoothing to the field's own derived smoother.
///
/// ## `#[smooth(map)]` — per-entry smoothing for `HashMap` fields
///
/// Smooths each entry of a `HashMap<K, V>` independently.
#[proc_macro_derive(Smooth, attributes(smooth))]
pub fn derive_smooth(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let name = &input.ident;
    let smoother_name = format_ident!("{}Smoother", name);
    let vis = &input.vis;

    let mut decay: f32 = 0.9;
    let mut max_offset: f32 = 200.0;
    let mut min_offset_sq: f32 = 4.0;
    let mut has_custom_params = false;

    for attr in &input.attrs {
        if attr.path().is_ident("smooth") {
            has_custom_params = true;
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

    let mut direct_fields = Vec::new();
    let mut nested_fields = Vec::new();
    let mut map_fields = Vec::new();

    for field in fields.iter() {
        if let Some(attr) = field.attrs.iter().find(|a| a.path().is_ident("smooth")) {
            let mut is_map = false;
            let mut is_nested = false;
            if let syn::Meta::List(_) = &attr.meta {
                let _ = attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("map") {
                        is_map = true;
                    } else if meta.path.is_ident("nested") {
                        is_nested = true;
                    }
                    Ok(())
                });
            }

            if is_map {
                map_fields.push(field);
            } else if is_nested {
                nested_fields.push(field);
            } else {
                direct_fields.push(field);
            }
        }
    }

    // --- smoother struct field definitions ---

    let direct_field_defs = direct_fields.iter().map(|f| {
        let name = &f.ident;
        let ty = &f.ty;
        quote! { pub #name: #ty }
    });

    let nested_field_defs = nested_fields.iter().map(|f| {
        let name = &f.ident;
        let ty = &f.ty;
        quote! {
            pub #name: <#ty as ::deform_core::Smoothable>::Smoother
        }
    });

    let map_field_defs = map_fields.iter().map(|f| {
        let name = &f.ident;
        let (key_ty, val_ty) = extract_map_kv(&f.ty);
        quote! {
            pub #name: std::collections::HashMap<#key_ty, <#val_ty as ::deform_core::Smoothable>::Smoother>
        }
    });

    // --- Default impl ---

    let direct_field_defaults = direct_fields.iter().map(|f| {
        let name = &f.ident;
        quote! { #name: Default::default() }
    });

    let nested_field_defaults = nested_fields.iter().map(|f| {
        let name = &f.ident;
        quote! { #name: Default::default() }
    });

    let map_field_defaults = map_fields.iter().map(|f| {
        let name = &f.ident;
        quote! { #name: Default::default() }
    });

    // --- reset ---

    let direct_reset = direct_fields.iter().map(|f| {
        let name = &f.ident;
        quote! { self.#name = Default::default(); }
    });

    let nested_reset = nested_fields.iter().map(|f| {
        let name = &f.ident;
        quote! { ::deform_core::Smooth::reset(&mut self.#name); }
    });

    let map_reset = map_fields.iter().map(|f| {
        let name = &f.ident;
        quote! { self.#name.clear(); }
    });

    // --- on_rollback ---

    let direct_rollback = direct_fields.iter().map(|f| {
        let name = &f.ident;
        quote! {
            {
                let mut pre_visual = pre.#name.clone();
                pre_visual += self.#name.clone();
                self.#name = pre_visual - post.#name.clone();
                if ::deform_core::SmoothableField::magnitude_sq(&self.#name) > self.__params.max_offset_sq {
                    self.#name = Default::default();
                }
            }
        }
    });

    let nested_rollback = nested_fields.iter().map(|f| {
        let name = &f.ident;
        quote! {
            ::deform_core::Smooth::on_rollback(&mut self.#name, &pre.#name, &post.#name);
        }
    });

    let map_rollback = map_fields.iter().map(|f| {
        let name = &f.ident;
        quote! {
            {
                let __params = self.__params;
                for (__key, __new_val) in &post.#name {
                    let __smoother = self.#name.entry(__key.clone()).or_insert_with(|| {
                        let mut __s = Default::default();
                        ::deform_core::Smooth::set_params(&mut __s, __params);
                        __s
                    });
                    if let Some(__old_val) = pre.#name.get(__key) {
                        ::deform_core::Smooth::on_rollback(__smoother, __old_val, __new_val);
                    } else {
                        ::deform_core::Smooth::reset(__smoother);
                    }
                }
                self.#name.retain(|__k, _| post.#name.contains_key(__k));
            }
        }
    });

    // --- apply (lerp + offset decay) ---

    let direct_apply = direct_fields.iter().map(|f| {
        let name = &f.ident;
        quote! {
            {
                let target = ::deform_core::SmoothableField::lerp_toward(&prev.#name, &current.#name, t);
                self.#name *= self.__params.decay;
                if ::deform_core::SmoothableField::magnitude_sq(&self.#name) < self.__params.min_offset_sq {
                    self.#name = Default::default();
                }
                current.#name = target + self.#name.clone();
            }
        }
    });

    let nested_apply = nested_fields.iter().map(|f| {
        let name = &f.ident;
        quote! {
            ::deform_core::Smooth::apply(&mut self.#name, &prev.#name, &mut current.#name, t);
        }
    });

    let map_apply = map_fields.iter().map(|f| {
        let name = &f.ident;
        quote! {
            {
                let __params = self.__params;
                for (__key, __current_val) in &mut current.#name {
                    let __smoother = self.#name.entry(__key.clone()).or_insert_with(|| {
                        let mut __s = Default::default();
                        ::deform_core::Smooth::set_params(&mut __s, __params);
                        __s
                    });
                    if let Some(__prev_val) = prev.#name.get(__key) {
                        ::deform_core::Smooth::apply(__smoother, __prev_val, __current_val, t);
                    }
                }
            }
        }
    });

    // --- set_params / scale_decay ---

    let nested_set_params_names: Vec<_> = nested_fields.iter().map(|f| &f.ident).collect();
    let map_field_names: Vec<_> = map_fields.iter().map(|f| &f.ident).collect();

    let expanded = quote! {
        #[derive(Clone)]
        #vis struct #smoother_name {
            #(#direct_field_defs,)*
            #(#nested_field_defs,)*
            #(#map_field_defs,)*
            __params: ::deform_core::SmoothParams,
            __custom_params: bool,
        }

        impl Default for #smoother_name {
            fn default() -> Self {
                Self {
                    #(#direct_field_defaults,)*
                    #(#nested_field_defaults,)*
                    #(#map_field_defaults,)*
                    __params: ::deform_core::SmoothParams {
                        decay: #decay,
                        max_offset_sq: #max_offset_sq,
                        min_offset_sq: #min_offset_sq,
                    },
                    __custom_params: #has_custom_params,
                }
            }
        }

        impl ::deform_core::Smooth<#name> for #smoother_name {
            fn reset(&mut self) {
                #(#direct_reset)*
                #(#nested_reset)*
                #(#map_reset)*
            }

            fn on_rollback(&mut self, pre: &#name, post: &#name) {
                #(#direct_rollback)*
                #(#nested_rollback)*
                #(#map_rollback)*
            }

            fn apply(&mut self, prev: &#name, current: &mut #name, t: f32) {
                #(#direct_apply)*
                #(#nested_apply)*
                #(#map_apply)*
            }

            fn scale_decay(&mut self, ratio: f32) {
                self.__params.decay = self.__params.decay.powf(ratio);
                #(::deform_core::Smooth::scale_decay(&mut self.#nested_set_params_names, ratio);)*
                #(for __smoother in self.#map_field_names.values_mut() {
                    ::deform_core::Smooth::scale_decay(__smoother, ratio);
                })*
            }

            fn set_params(&mut self, params: ::deform_core::SmoothParams) {
                if !self.__custom_params {
                    self.__params = params;
                }
                #(::deform_core::Smooth::set_params(&mut self.#nested_set_params_names, params);)*
            }
        }

        impl ::deform_core::Smoothable for #name {
            type Smoother = #smoother_name;
        }
    };

    TokenStream::from(expanded)
}

fn extract_map_kv(ty: &Type) -> (&Type, &Type) {
    let Type::Path(type_path) = ty else {
        panic!("#[smooth(map)] field must be HashMap<K, V>");
    };
    let segment = type_path
        .path
        .segments
        .last()
        .expect("#[smooth(map)] field has empty type path");
    let PathArguments::AngleBracketed(ref args) = segment.arguments else {
        panic!("#[smooth(map)] field must have generic arguments");
    };
    let mut types = args.args.iter().filter_map(|a| match a {
        GenericArgument::Type(ty) => Some(ty),
        _ => None,
    });
    let k = types
        .next()
        .expect("#[smooth(map)] HashMap missing key type");
    let v = types
        .next()
        .expect("#[smooth(map)] HashMap missing value type");
    (k, v)
}
