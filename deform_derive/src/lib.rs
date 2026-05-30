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
/// All parameters are optional. Omitting `#[smooth(...)]` entirely uses the defaults.
///
/// # Field attributes
///
/// ## `#[smooth]` — direct field smoothing
///
/// Marks a field for offset-based smoothing. The field type must implement
/// [`SmoothableField`] and support `-`, `+=`, and `*= f32`.
///
/// Built-in types that work: `f32`, `f64`, `Vec2`, `Vec3`.
///
/// ```ignore
/// #[derive(Smooth)]
/// #[smooth(decay = 0.9, max_offset = 200.0, min_offset_sq = 4.0)]
/// struct GameState {
///     #[smooth]
///     ball_pos: Vec2,   // smoothed
///     ball_vel: Vec2,   // NOT smoothed — no #[smooth]
///     score: u32,       // NOT smoothed
/// }
/// ```
///
/// ## `#[smooth(map)]` — per-entry smoothing for `HashMap` fields
///
/// Smooths each entry of a `HashMap<K, V>` independently. `V` must also derive
/// `Smooth` (i.e. implement [`Smoothable`]). Entries are created and cleaned up
/// automatically as keys appear and disappear from the map.
///
/// ```ignore
/// #[derive(Smooth)]
/// struct PlayerState {
///     #[smooth]
///     position: Vec2,
///     score: u32,
/// }
///
/// #[derive(Smooth)]
/// #[smooth(decay = 0.9, max_offset = 200.0, min_offset_sq = 4.0)]
/// struct GameState {
///     #[smooth]
///     ball_pos: Vec2,
///     #[smooth(map)]
///     players: HashMap<Pubkey, PlayerState>,
/// }
/// ```
///
/// # Parameter inheritance
///
/// When a struct is used as a map value via `#[smooth(map)]`, the parent's
/// smoothing parameters are passed down through [`Smooth::set_params`].
///
/// - If the child has **no** `#[smooth(...)]`, it inherits the parent's parameters.
/// - If the child has **its own** `#[smooth(...)]`, it keeps them — `set_params` is a no-op.
///
/// ```ignore
/// // Inherits parent's decay/max_offset/min_offset_sq
/// #[derive(Smooth)]
/// struct PlayerState {
///     #[smooth]
///     position: Vec2,
/// }
///
/// // Keeps its own parameters, ignoring the parent's
/// #[derive(Smooth)]
/// #[smooth(decay = 0.5)]
/// struct HeavyPlayerState {
///     #[smooth]
///     position: Vec2,
/// }
/// ```
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
    let mut map_fields = Vec::new();

    for field in fields.iter() {
        if let Some(attr) = field.attrs.iter().find(|a| a.path().is_ident("smooth")) {
            let is_map = if let syn::Meta::List(_) = &attr.meta {
                let mut found = false;
                let _ = attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("map") {
                        found = true;
                    }
                    Ok(())
                });
                found
            } else {
                false
            };

            if is_map {
                map_fields.push(field);
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

    let map_field_defs = map_fields.iter().map(|f| {
        let name = &f.ident;
        let (key_ty, val_ty) = extract_map_kv(&f.ty);
        quote! {
            pub #name: std::collections::HashMap<#key_ty, <#val_ty as ::deform_core::Smoothable>::Smoother>
        }
    });

    // --- Default impl (sets __params from struct-level annotation) ---

    let direct_field_defaults = direct_fields.iter().map(|f| {
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

    let map_reset = map_fields.iter().map(|f| {
        let name = &f.ident;
        quote! { self.#name.clear(); }
    });

    // --- on_rollback ---

    let direct_rollback = direct_fields.iter().map(|f| {
        let name = &f.ident;
        quote! {
            {
                let mut pre_visual = old.#name.clone();
                pre_visual += self.#name.clone();
                self.#name = pre_visual - new.#name.clone();
                if ::deform_core::SmoothableField::magnitude_sq(&self.#name) > self.__params.max_offset_sq {
                    self.#name = Default::default();
                }
            }
        }
    });

    let map_rollback = map_fields.iter().map(|f| {
        let name = &f.ident;
        quote! {
            {
                let __params = self.__params;
                for (__key, __new_val) in &new.#name {
                    let __smoother = self.#name.entry(__key.clone()).or_insert_with(|| {
                        let mut __s = Default::default();
                        ::deform_core::Smooth::set_params(&mut __s, __params);
                        __s
                    });
                    if let Some(__old_val) = old.#name.get(__key) {
                        ::deform_core::Smooth::on_rollback(__smoother, __old_val, __new_val);
                    } else {
                        ::deform_core::Smooth::reset(__smoother);
                    }
                }
                self.#name.retain(|__k, _| new.#name.contains_key(__k));
            }
        }
    });

    // --- apply ---

    let direct_apply = direct_fields.iter().map(|f| {
        let name = &f.ident;
        quote! {
            self.#name *= self.__params.decay;
            if ::deform_core::SmoothableField::magnitude_sq(&self.#name) < self.__params.min_offset_sq {
                self.#name = Default::default();
            }
            state.#name += self.#name.clone();
        }
    });

    let map_apply = map_fields.iter().map(|f| {
        let name = &f.ident;
        quote! {
            for (__key, __val) in &mut state.#name {
                if let Some(__smoother) = self.#name.get_mut(__key) {
                    ::deform_core::Smooth::apply(__smoother, __val);
                }
            }
        }
    });

    let expanded = quote! {
        #[derive(Clone)]
        #vis struct #smoother_name {
            #(#direct_field_defs,)*
            #(#map_field_defs,)*
            __params: ::deform_core::SmoothParams,
            __custom_params: bool,
        }

        impl Default for #smoother_name {
            fn default() -> Self {
                Self {
                    #(#direct_field_defaults,)*
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
                #(#map_reset)*
            }

            fn on_rollback(&mut self, old: &#name, new: &#name) {
                #(#direct_rollback)*
                #(#map_rollback)*
            }

            fn apply(&mut self, state: &mut #name) {
                #(#direct_apply)*
                #(#map_apply)*
            }

            fn set_params(&mut self, params: ::deform_core::SmoothParams) {
                if !self.__custom_params {
                    self.__params = params;
                }
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
