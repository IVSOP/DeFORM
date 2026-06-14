use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, Data, DeriveInput, Fields, GenericArgument, PathArguments, Type};

/// Derives a companion smoother struct and implements [`Smooth<T>`] for it.
///
/// For a struct named `Foo`, this generates:
/// - `FooSmoother` — a unit struct
/// - `impl Smooth<Foo> for FooSmoother` — lerps `#[smooth]` fields between prev and current
/// - `impl Smoothable for Foo` — links `Foo` to its smoother for composition
///
/// # Field attributes
///
/// ## `#[smooth]` — direct field interpolation
///
/// Marks a field for lerp-based interpolation. The field type must implement
/// [`SmoothableField`].
///
/// Built-in types that work: `f32`, `f64`, `Vec2`, `Vec3`.
///
/// ```ignore
/// #[derive(Smooth)]
/// struct GameState {
///     #[smooth]
///     ball_pos: Vec2,   // interpolated
///     ball_vel: Vec2,   // NOT interpolated — no #[smooth]
///     score: u32,       // NOT interpolated
/// }
/// ```
///
/// ## `#[smooth(nested)]` — delegate to a child smoother
///
/// Delegates interpolation to the field's own derived smoother. The field type
/// must also derive `Smooth` (i.e. implement [`Smoothable`]).
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
/// struct GameState {
///     #[smooth(nested)]
///     player: PlayerState,
/// }
/// ```
///
/// ## `#[smooth(map)]` — per-entry interpolation for `HashMap` fields
///
/// Interpolates each entry of a `HashMap<K, V>` independently. `V` must also
/// derive `Smooth` (i.e. implement [`Smoothable`]). Entries that only exist in
/// `current` (new keys) are left as-is (snap).
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
/// struct GameState {
///     #[smooth]
///     ball_pos: Vec2,
///     #[smooth(map)]
///     players: HashMap<Pubkey, PlayerState>,
/// }
/// ```
#[proc_macro_derive(Smooth, attributes(smooth))]
pub fn derive_smooth(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let name = &input.ident;
    let smoother_name = format_ident!("{}Smoother", name);
    let vis = &input.vis;

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

    // --- apply (lerp) ---

    let direct_apply = direct_fields.iter().map(|f| {
        let name = &f.ident;
        quote! {
            current.#name = ::deform_core::SmoothableField::lerp_toward(&prev.#name, &current.#name, t);
        }
    });

    let nested_apply = nested_fields.iter().map(|f| {
        let name = &f.ident;
        let ty = &f.ty;
        quote! {
            <<#ty as ::deform_core::Smoothable>::Smoother as ::deform_core::Smooth<#ty>>::apply(
                &prev.#name, &mut current.#name, t
            );
        }
    });

    let map_apply = map_fields.iter().map(|f| {
        let name = &f.ident;
        let (_key_ty, val_ty) = extract_map_kv(&f.ty);
        quote! {
            for (__key, __current_val) in &mut current.#name {
                if let Some(__prev_val) = prev.#name.get(__key) {
                    <<#val_ty as ::deform_core::Smoothable>::Smoother as ::deform_core::Smooth<#val_ty>>::apply(
                        __prev_val, __current_val, t
                    );
                }
            }
        }
    });

    let expanded = quote! {
        #[derive(Default, Clone)]
        #vis struct #smoother_name;

        impl ::deform_core::Smooth<#name> for #smoother_name {
            fn apply(prev: &#name, current: &mut #name, t: f32) {
                #(#direct_apply)*
                #(#nested_apply)*
                #(#map_apply)*
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
