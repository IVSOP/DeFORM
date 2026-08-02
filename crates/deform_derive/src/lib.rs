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
/// | `decay`        | `0.9`   | Multiplier applied to offsets each *simulation tick* (lower = faster snap) |
/// | `max_offset`   | `200.0` | Discontinuity threshold: rollback offsets larger than this are discarded, and single-tick jumps larger than this snap instead of interpolating |
/// | `min_offset_sq`| `4.0`   | Offsets with squared magnitude below this are zeroed out      |
/// | `max_correction`| unset  | Max distance an offset is pulled toward zero per *simulation tick*, on top of `decay`. Unset = pure exponential decay |
/// | `motion_ratio` | unset   | Caps the offset at this multiple of the distance the field moved this tick, so a field at rest snaps instead of drifting. Dimensionless |
///
/// `max_offset` must sit above the largest distance a field covers in one tick
/// during normal play, and below the smallest genuine teleport.
///
/// `decay` alone is asymptotic: while mispredictions keep arriving, the offset
/// settles at `e / (1 - decay)` for a per-tick error `e` and never visibly ends.
/// Set `max_correction` to bound how long any correction can last — a good starting
/// point is `worst_case_offset / ticks_you_are_willing_to_spend`.
///
/// `motion_ratio` targets the worst case for the eye: a correction that outlives the
/// motion it was hiding inside. Reach for it when a remote entity that has stopped
/// keeps visibly gliding.
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
    let mut max_correction: Option<f32> = None;
    let mut motion_ratio: Option<f32> = None;
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
                } else if meta.path.is_ident("max_correction") {
                    let value = meta.value()?;
                    let lit: Lit = value.parse()?;
                    if let Lit::Float(f) = &lit {
                        max_correction = Some(f.base10_parse()?);
                    } else if let Lit::Int(i) = &lit {
                        max_correction = Some(i.base10_parse()?);
                    }
                } else if meta.path.is_ident("motion_ratio") {
                    let value = meta.value()?;
                    let lit: Lit = value.parse()?;
                    if let Lit::Float(f) = &lit {
                        motion_ratio = Some(f.base10_parse()?);
                    } else if let Lit::Int(i) = &lit {
                        motion_ratio = Some(i.base10_parse()?);
                    }
                }
                Ok(())
            })
            .unwrap_or_else(|e| panic!("failed to parse #[smooth(...)]: {e}"));
        }
    }

    let max_offset_sq = max_offset * max_offset;
    // Unset means "pure exponential decay", i.e. no bound.
    let max_correction = match max_correction {
        Some(v) => quote! { #v },
        None => quote! { f32::INFINITY },
    };
    // Unset means "offsets are never capped by how much the field is moving".
    let motion_ratio = match motion_ratio {
        Some(v) => quote! { #v },
        None => quote! { f32::INFINITY },
    };

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
                if ::deform_core::SmoothableField::magnitude_sq(&self.#name) > self.__scaled.max_offset_sq {
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
                let __scale = self.__scale;
                for (__key, __new_val) in &post.#name {
                    let __smoother = self.#name.entry(__key.clone()).or_insert_with(|| {
                        let mut __s = Default::default();
                        // Inherit, then convert to visual-frame units. Both steps are
                        // needed: `set_params` is a no-op for a child with its own
                        // `#[smooth(...)]`, but that child still has to be scaled.
                        ::deform_core::Smooth::set_params(&mut __s, __params);
                        ::deform_core::Smooth::scale_decay(&mut __s, __scale);
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
                // A single-tick jump larger than `max_offset` is a discontinuity
                // (respawn, round reset, warp), not motion. Lerping across it would
                // drag the entity over the whole gap, so snap and drop any residual
                // offset instead.
                let __jump = current.#name.clone() - prev.#name.clone();
                let __jump_sq = ::deform_core::SmoothableField::magnitude_sq(&__jump);
                let __jump_mag = __jump_sq.sqrt();
                if __jump_sq > self.__scaled.max_offset_sq {
                    self.#name = Default::default();
                } else {
                    let target = ::deform_core::SmoothableField::lerp_toward(&prev.#name, &current.#name, t);
                    self.#name *= self.__scaled.decay;
                    // Exponential decay alone is asymptotic, so while fresh
                    // mispredictions keep re-injecting an offset the correction never
                    // visibly ends — it just settles at `e / (1 - decay)` and drifts.
                    // Subtracting a fixed step as well bounds any correction to
                    // `magnitude / max_correction` ticks.
                    if self.__scaled.max_correction.is_finite() {
                        let __mag_sq = ::deform_core::SmoothableField::magnitude_sq(&self.#name);
                        if __mag_sq > 0.0 {
                            let __mag = __mag_sq.sqrt();
                            self.#name *= (__mag - self.__scaled.max_correction).max(0.0) / __mag;
                        }
                    }
                    // An offset is only invisible while it hides inside real motion.
                    // Once the true state comes to rest, whatever is left renders as
                    // movement that is not happening — the opponent who keeps sliding
                    // after they stop. Allowing only a multiple of the distance actually
                    // travelled this tick makes a stopped field snap and leaves a moving
                    // one free to smooth.
                    if self.__scaled.motion_ratio.is_finite() {
                        let __allowed = __jump_mag * self.__scaled.motion_ratio;
                        let __mag_sq = ::deform_core::SmoothableField::magnitude_sq(&self.#name);
                        if __mag_sq > __allowed * __allowed {
                            let __mag = __mag_sq.sqrt();
                            self.#name *= __allowed / __mag;
                        }
                    }
                    if ::deform_core::SmoothableField::magnitude_sq(&self.#name) < self.__scaled.min_offset_sq {
                        self.#name = Default::default();
                    }
                    current.#name = target + self.#name.clone();
                }
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
                let __scale = self.__scale;
                for (__key, __current_val) in &mut current.#name {
                    let __smoother = self.#name.entry(__key.clone()).or_insert_with(|| {
                        let mut __s = Default::default();
                        // See `on_rollback`: inherit, then scale.
                        ::deform_core::Smooth::set_params(&mut __s, __params);
                        ::deform_core::Smooth::scale_decay(&mut __s, __scale);
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
            /// Authored on this type, or inherited from the parent. Always in
            /// *per simulation tick* units — never scaled in place, so it stays
            /// replayable for children created after `scale_decay` has run.
            __params: ::deform_core::SmoothParams,
            /// `visual_tick_micros / sim_tick_micros`.
            __scale: f32,
            /// `__params` converted to per-visual-frame units. The hot paths read this.
            __scaled: ::deform_core::SmoothParams,
            __custom_params: bool,
        }

        impl #smoother_name {
            /// Re-derives the per-frame params from the per-tick ones. `decay` is a
            /// rate and `max_correction` a distance-per-tick, so both convert; the
            /// offset thresholds are plain distances and carry over untouched.
            fn __refresh(&mut self) {
                self.__scaled = ::deform_core::SmoothParams {
                    decay: self.__params.decay.powf(self.__scale),
                    max_correction: self.__params.max_correction * self.__scale,
                    ..self.__params
                };
            }
        }

        impl Default for #smoother_name {
            fn default() -> Self {
                let __authored = ::deform_core::SmoothParams {
                    decay: #decay,
                    max_offset_sq: #max_offset_sq,
                    min_offset_sq: #min_offset_sq,
                    max_correction: #max_correction,
                    motion_ratio: #motion_ratio,
                };
                let mut __s = Self {
                    #(#direct_field_defaults,)*
                    #(#nested_field_defaults,)*
                    #(#map_field_defaults,)*
                    __params: __authored,
                    __scale: 1.0,
                    __scaled: __authored,
                    __custom_params: #has_custom_params,
                };
                // Nested children build themselves from `Default` and so start on the
                // *derive* defaults. Push ours down so `#[smooth(nested)]` inherits like
                // `#[smooth(map)]` already does at insertion time; a child that authored
                // its own `#[smooth(...)]` ignores this.
                #(::deform_core::Smooth::set_params(&mut __s.#nested_set_params_names, __authored);)*
                __s
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
                // Assigned rather than accumulated, so scaling twice — or scaling a map
                // entry that was created after the parent was already scaled — is idempotent.
                self.__scale = ratio;
                self.__refresh();
                #(::deform_core::Smooth::scale_decay(&mut self.#nested_set_params_names, ratio);)*
                #(for __smoother in self.#map_field_names.values_mut() {
                    ::deform_core::Smooth::scale_decay(__smoother, ratio);
                })*
            }

            fn set_params(&mut self, params: ::deform_core::SmoothParams) {
                if !self.__custom_params {
                    self.__params = params;
                    self.__refresh();
                }
                // Forward our *effective* params, not the incoming ones: a type that
                // authored its own `#[smooth(...)]` overrides for its whole subtree, not
                // just for itself. Map entries are included so re-parameterizing a parent
                // reaches entries that already exist.
                let __effective = self.__params;
                #(::deform_core::Smooth::set_params(&mut self.#nested_set_params_names, __effective);)*
                #(for __smoother in self.#map_field_names.values_mut() {
                    ::deform_core::Smooth::set_params(__smoother, __effective);
                })*
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
