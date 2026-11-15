use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{
    Data, DeriveInput, FnArg, GenericParam, Index, ItemFn, Member, Path, Type, parse_macro_input,
    parse_quote, spanned::Spanned,
};

mod param_impl;

fn linked_crates() -> proc_macro2::TokenStream {
    let mut args = Vec::new();
    for arg in std::env::args_os().filter_map(|arg| arg.into_string().ok()) {
        match arg.strip_prefix('@') {
            Some(path) => args.extend(
                std::fs::read_to_string(path)
                    .unwrap_or_default()
                    .lines()
                    .map(String::from),
            ),
            None => args.push(arg),
        }
    }

    let mut names: Vec<&str> = args
        .windows(2)
        .filter(|pair| pair[0] == "--extern")
        .filter_map(|pair| pair[1].split('=').next()?.rsplit(':').next())
        .collect();
    names.sort_unstable();
    names.dedup();

    let names = names
        .into_iter()
        .filter_map(|name| syn::parse_str::<syn::Ident>(name).ok());

    quote! { #( use #names as _; )* }
}

/// Returns a copy of a type's name for the code a macro generates around the type.
///
/// rust-analyzer shows one hover for each token in a macro's output that points at the hovered
/// name, so reusing the name token in generated impls repeats the type's hover. The copy points at
/// the macro call instead, and `mixed_site` resolves an item name exactly as the source name would.
fn generated_name(ident: &syn::Ident) -> syn::Ident {
    syn::Ident::new(&ident.to_string(), proc_macro2::Span::mixed_site())
}

fn attr_named<'a>(attrs: &'a [syn::Attribute], names: &[&str]) -> Option<&'a syn::Attribute> {
    attrs.iter().find(|attr| {
        attr.path()
            .segments
            .last()
            .is_some_and(|segment| names.iter().any(|name| segment.ident == name))
    })
}

const DATA_DERIVES: &[&str] = &["Clone", "Copy", "Default", "Serialize", "Deserialize"];
const TYPE_DERIVES: &[&str] = &[
    "Clone",
    "Copy",
    "Default",
    "Hashed",
    "DeriveHashed",
    "Serialize",
    "Deserialize",
];
const RESOURCE_DERIVES: &[&str] = &[
    "Default",
    "Hashed",
    "DeriveHashed",
    "Serialize",
    "Deserialize",
];

fn derived_path(attrs: &[syn::Attribute], names: &[&str]) -> Option<Path> {
    let parser = syn::punctuated::Punctuated::<Path, syn::Token![,]>::parse_terminated;
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("derive"))
        .filter_map(|attr| attr.parse_args_with(parser).ok())
        .flatten()
        .find(|path| {
            path.segments
                .last()
                .is_some_and(|last| names.iter().any(|name| last.ident == name))
        })
}

fn reject_derives(
    attrs: &[syn::Attribute],
    macro_name: &str,
    derived: &[&str],
) -> Option<TokenStream> {
    let path = derived_path(attrs, derived)?;
    let name = &path.segments.last()?.ident;
    let message = format!("remove `{name}` from this derive: {macro_name} already derives it");
    Some(
        syn::Error::new(path.span(), message)
            .to_compile_error()
            .into(),
    )
}

fn reject_data(attrs: &[syn::Attribute], macro_name: &str) -> Option<TokenStream> {
    let attr = attr_named(attrs, &["data"])?;
    let message = format!("remove #[data]: {macro_name} already makes this type serializable");
    Some(
        syn::Error::new(attr.span(), message)
            .to_compile_error()
            .into(),
    )
}

#[proc_macro]
pub fn world(input: TokenStream) -> TokenStream {
    parse_macro_input!(input as syn::parse::Nothing);
    let link = linked_crates();

    TokenStream::from(quote! {
        {
            #link
            ::reacs::world::World::new()
        }
    })
}

#[proc_macro_derive(Hashed)]
pub fn derive_hashed_def(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let name_str = name.to_string();

    let mut generics = input.generics.clone();
    for tp in generics.type_params_mut() {
        tp.bounds.push(parse_quote!(::reacs::hashed::Hashed));
    }
    let (impl_generics, _, where_clause) = generics.split_for_impl();
    let (_, ty_generics, _) = input.generics.split_for_impl();

    let mixins: Vec<_> = input
        .generics
        .params
        .iter()
        .filter_map(|p| match p {
            GenericParam::Type(t) => {
                let id = &t.ident;
                Some(quote!(<#id as ::reacs::hashed::Hashed>::TYPE_UUID))
            }
            GenericParam::Const(c) => {
                let id = &c.ident;
                Some(quote!((#id as u128)))
            }
            GenericParam::Lifetime(_) => None,
        })
        .collect();

    let expanded = quote! {
        impl #impl_generics ::reacs::hashed::Hashed for #name #ty_generics #where_clause {
            const TYPE_UUID: u128 = {
                let h: u128 = ::reacs::hashed::hash_path(::core::module_path!(), #name_str);
                #( let h = (h ^ #mixins).wrapping_mul(0x0000000001000000000000000000013B); )*
                h
            };
        }
    };

    TokenStream::from(expanded)
}

#[proc_macro_attribute]
pub fn resource(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut local = false;
    let mut on_load: Option<syn::Ident> = None;
    let attr_parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("local") {
            local = true;
            Ok(())
        } else if meta.path.is_ident("on_load") {
            on_load = Some(meta.value()?.parse()?);
            Ok(())
        } else {
            Err(meta.error("expected `local` or `on_load = method`"))
        }
    });
    parse_macro_input!(attr with attr_parser);

    if let (Some(hook), false) = (&on_load, local) {
        return syn::Error::new(hook.span(), "`on_load` requires `local`")
            .to_compile_error()
            .into();
    }

    let input = parse_macro_input!(item as DeriveInput);
    let name = generated_name(&input.ident);

    if let Some(attr) = attr_named(&input.attrs, &["data"]) {
        let message = if local {
            "remove #[data]: a local resource is never saved, so it doesn't need serde"
        } else {
            "remove #[data]: #[resource] already makes this type serializable"
        };
        return syn::Error::new(attr.span(), message)
            .to_compile_error()
            .into();
    }

    let derived = if local {
        reject_derives(&input.attrs, "#[resource(local)]", &["Default"])
    } else {
        reject_derives(&input.attrs, "#[resource]", RESOURCE_DERIVES)
    };
    if let Some(error) = derived {
        return error;
    }

    if !input.generics.params.is_empty() {
        let message = if local {
            "#[resource(local)] does not support generic resources"
        } else {
            "#[resource] does not support generic resources"
        };
        return syn::Error::new(input.generics.span(), message)
            .to_compile_error()
            .into();
    }

    let snake_name = format_ident!("{}", name.to_string().to_lowercase());
    let ctor_fn = format_ident!("__register_type_{}", snake_name);
    let link = linked_crates();

    if local {
        let on_load = on_load.map(|hook| {
            quote! {
                const ON_LOAD: ::core::option::Option<fn(&mut Self, &::reacs::world::World)> =
                    ::core::option::Option::Some(Self::#hook);
            }
        });

        return TokenStream::from(quote! {
            #[derive(::core::default::Default)]
            #input

            impl ::reacs::resource::LocalResourceType for #name {
                #on_load
            }

            const _: () = {
                #link
            };
        });
    }

    let expanded = quote! {
        #[derive(
            ::core::default::Default,
            ::reacs::hashed::Hashed,
            ::reacs::__private::serde::Serialize,
            ::reacs::__private::serde::Deserialize,
        )]
        #[serde(crate = "::reacs::__private::serde")]
        #input

        #[::reacs::__private::ctor::ctor(unsafe, naked, crate_path = ::reacs::__private::ctor)]
        fn #ctor_fn() {
            #link
            ::reacs::registry::register_entry::<#name>(
                <#name as ::reacs::hashed::Hashed>::TYPE_UUID
            );
        }
    };

    TokenStream::from(expanded)
}

#[proc_macro_attribute]
pub fn component(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    let name = generated_name(&input.ident);

    if let Some(error) = reject_data(&input.attrs, "#[component]")
        .or_else(|| reject_derives(&input.attrs, "#[component]", TYPE_DERIVES))
    {
        return error;
    }

    if !input.generics.params.is_empty() {
        return syn::Error::new(
            input.generics.span(),
            "#[component] does not support generic components",
        )
        .to_compile_error()
        .into();
    }

    let snake_name = format_ident!("{}", name.to_string().to_lowercase());
    let ctor_fn = format_ident!("__register_type_{}", snake_name);
    let link = linked_crates();

    let expanded = quote! {
        #[derive(
            ::core::default::Default,
            ::reacs::hashed::Hashed,
            ::core::clone::Clone,
            ::core::marker::Copy,
            ::reacs::__private::serde::Serialize,
            ::reacs::__private::serde::Deserialize,
        )]
        #[serde(crate = "::reacs::__private::serde")]
        #input

        #[::reacs::__private::ctor::ctor(unsafe, naked, crate_path = ::reacs::__private::ctor)]
        #[allow(non_snake_case)]
        fn #ctor_fn() {
        #link
        type Map = ::reacs::component::ComponentMap<#name>;
        ::reacs::registry::register_entry::<Map>(
            <Map as ::reacs::hashed::Hashed>::TYPE_UUID,
        );
        }
    };

    TokenStream::from(expanded)
}

/// Makes your own type usable as a field of components, signals and resources.
///
/// Everything in a world is saved, loaded and hashed, so every field type must implement serde's
/// `Serialize` and `Deserialize`. `#[component]`, `#[signal]` and `#[resource]` derive them for the
/// types they declare, but a type used as a field, like an enum or a small struct, needs them too.
/// `#[data]` derives both through reacs, so your crate doesn't need serde as a dependency.
///
/// The macro derives `Clone`, `Copy`, `Default`, `Serialize` and `Deserialize`, like `#[component]`
/// and `#[signal]`, so don't derive them yourself; other derives, like `PartialEq` or `Debug`, are
/// fine. An enum needs `#[default]` on one variant. Generic types are supported, and so are serde's
/// field attributes.
///
/// A field marked `#[serde(skip)]` isn't saved or hashed: after a load it holds its `Default`
/// value. Like a local resource, it may only hold a cache the simulation can rebuild. If game logic
/// reads it, a loaded world or a replay gives different results, and recorded hashes only notice
/// once the difference reaches saved data.
///
/// `#[data(no_copy)]` leaves out `Copy`, for types holding a `Vec`, a `String` or an `IndexMap`.
/// Components and signals must be `Copy`, so those types can only be used in resources.
///
/// Types declared with `#[component]`, `#[signal]` or `#[resource]` are already serializable and
/// can be used as fields without `#[data]`. A type from another crate, like a math library's
/// vector, can't take `#[data]`; it works as a field when that crate implements serde, usually
/// behind a `serde` feature you enable in your `Cargo.toml`.
///
/// Changing a `#[data]` type, like adding or reordering enum variants, changes what saved data
/// means, just as changing a component does: raise the app version (`World::set_app_version`) in
/// that release.
///
/// # Compile errors
///
/// `#[data]` together with `#[component]`, `#[signal]` or `#[resource]` on the same type is an
/// error, since those already derive serde (a local resource is never saved and doesn't need it).
/// So is deriving a trait `#[data]` already derives, or `Copy` with `no_copy`.
///
/// A field type without `#[data]` fails to compile with errors saying it doesn't implement
/// `Serialize`, `Deserialize`, `Default`, `Clone` or `Copy`. The compiler suggests deriving those
/// traits; with reacs, add `#[data]` instead.
///
/// # Examples
///
/// ```standalone_crate
/// use reacs::prelude::*;
///
/// #[data]
/// #[derive(PartialEq, Debug)]
/// enum Element {
///     #[default]
///     Fire,
///     Ice,
/// }
///
/// #[data]
/// struct Hit {
///     element: Element,
///     amount: i32,
/// }
///
/// #[component]
/// struct Weakness(Element);
///
/// #[signal(entity)]
/// struct Damage {
///     target: Entity,
///     hit: Hit,
/// }
///
/// #[resource]
/// struct Log {
///     hits: Vec<Hit>,
/// }
///
/// #[system]
/// fn take_damage(
///     mut damage: Mut<Receiver<Damage>>,
///     weakness: Component<Weakness>,
///     mut log: Mut<Resource<Log>>,
/// ) {
///     for damage in damage.receive() {
///         let mut hit = damage.hit;
///         if weakness.get(damage.target).is_some_and(|weak| weak.0 == hit.element) {
///             hit.amount *= 2;
///         }
///         log.hits.push(hit);
///     }
/// }
///
/// fn main() {
///     let mut world = World::build(|setup| {
///         let ship = setup.spawn();
///         setup.set_component(ship, Weakness(Element::Ice));
///         let hit = Hit { element: Element::Ice, amount: 5 };
///         setup.signal::<Damage>().emit(Damage { target: ship, hit });
///     });
///     world.step();
///
///     let save = world.save();
///     world.load(&save);
///     let hit = world.get_resource::<Log>().unwrap().hits[0];
///     assert_eq!((hit.element, hit.amount), (Element::Ice, 10));
/// }
/// ```
///
/// A type holding a `Vec`, used in a resource:
///
/// ```standalone_crate
/// use reacs::prelude::*;
///
/// #[data(no_copy)]
/// struct Squad {
///     members: Vec<Entity>,
/// }
///
/// #[resource]
/// struct Army {
///     squads: IndexMap<u32, Squad>,
/// }
///
/// fn main() {
///     let mut world = World::build(|setup| {
///         let soldier = setup.spawn();
///         let squad = Squad { members: vec![soldier] };
///         setup.insert_resource(Army { squads: IndexMap::from([(7, squad)]) });
///     });
///
///     let save = world.save();
///     world.load(&save);
///     assert_eq!(world.get_resource::<Army>().unwrap().squads[&7].members.len(), 1);
/// }
/// ```
#[proc_macro_attribute]
pub fn data(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut no_copy = false;
    let attr_parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("no_copy") {
            no_copy = true;
            Ok(())
        } else {
            Err(meta.error("unsupported #[data] argument, expected `no_copy`"))
        }
    });
    parse_macro_input!(attr with attr_parser);
    let input = parse_macro_input!(item as DeriveInput);

    if let Some(attr) = attr_named(&input.attrs, &["component", "signal", "resource"]) {
        let name = attr.path().segments.last().unwrap().ident.to_string();
        let local = matches!(&attr.meta, syn::Meta::List(list)
            if list.tokens.clone().into_iter().any(|token| token.to_string() == "local"));
        let message = if local {
            "remove #[data]: a local resource is never saved, so it doesn't need serde".to_string()
        } else {
            format!("remove #[data]: #[{name}] already makes this type serializable")
        };
        return syn::Error::new(proc_macro2::Span::call_site(), message)
            .to_compile_error()
            .into();
    }

    let derived = if no_copy {
        let copy = derived_path(&input.attrs, &["Copy"]).map(|path| {
            let message = "remove `Copy` from this derive, or `no_copy` from #[data]";
            syn::Error::new(path.span(), message)
                .to_compile_error()
                .into()
        });
        copy.or_else(|| {
            reject_derives(
                &input.attrs,
                "#[data(no_copy)]",
                &["Clone", "Default", "Serialize", "Deserialize"],
            )
        })
    } else {
        reject_derives(&input.attrs, "#[data]", DATA_DERIVES)
    };
    if let Some(error) = derived {
        return error;
    }

    let copy = (!no_copy).then(|| quote! { ::core::marker::Copy, });
    let link = linked_crates();

    TokenStream::from(quote! {
        #[derive(
            ::core::clone::Clone,
            #copy
            ::core::default::Default,
            ::reacs::__private::serde::Serialize,
            ::reacs::__private::serde::Deserialize,
        )]
        #[serde(crate = "::reacs::__private::serde")]
        #input

        const _: () = {
            #link
        };
    })
}

#[proc_macro_attribute]
pub fn system(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut run_before: Vec<Path> = Vec::new();
    let mut run_after: Vec<Path> = Vec::new();
    let mut in_group: Vec<Path> = Vec::new();
    let attr_parser = syn::meta::parser(|meta| {
        let targets = if meta.path.is_ident("run_before") {
            &mut run_before
        } else if meta.path.is_ident("run_after") {
            &mut run_after
        } else if meta.path.is_ident("in_group") {
            &mut in_group
        } else {
            return Err(
                meta.error("expected `run_before(...)`, `run_after(...)` or `in_group(...)`")
            );
        };
        meta.parse_nested_meta(|target| {
            targets.push(target.path);
            Ok(())
        })
    });
    parse_macro_input!(attr with attr_parser);

    let input = parse_macro_input!(item as ItemFn);
    let vis = &input.vis;
    let name = &input.sig.ident;
    let name_str = name.to_string();
    let ctor_fn = format_ident!("__register_system_{}", name);
    let link = linked_crates();

    let mut param_types = Vec::new();
    for arg in &input.sig.inputs {
        match arg {
            FnArg::Typed(pat_type) => param_types.push(pat_type.ty.clone()),
            FnArg::Receiver(receiver) => {
                return syn::Error::new(
                    receiver.span(),
                    "#[system] does not support methods with `self`",
                )
                .to_compile_error()
                .into();
            }
        }
    }

    let expanded = quote! {
        #input

        #[allow(non_camel_case_types, dead_code)]
        #vis enum #name {}

        impl ::reacs::schedule::SystemId for #name {
            const UUID: u128 = ::reacs::hashed::hash_path(::core::module_path!(), #name_str);
        }

        #[::reacs::__private::ctor::ctor(unsafe, naked, crate_path = ::reacs::__private::ctor)]
        #[allow(non_snake_case)]
        fn #ctor_fn() {
            #link
            #( let _ = #run_before; )*
            #( let _ = #run_after; )*
            #( let _ = #in_group; )*
            let f: for<'w> fn(#(<#param_types as ::reacs::schedule::SystemArg>::Item<'w>),*) = #name;
            let system = ::reacs::schedule::IntoSystem::into_system(f)
                .named(::core::module_path!(), #name_str)
                .run_before(&[#(<#run_before as ::reacs::schedule::SystemId>::UUID),*])
                .run_after(&[#(<#run_after as ::reacs::schedule::SystemId>::UUID),*])
                .in_group(&[#(<#in_group as ::reacs::schedule::SystemId>::UUID),*]);
            ::reacs::schedule::register_system(system);
        }
    };

    TokenStream::from(expanded)
}

#[proc_macro_attribute]
pub fn param(attr: TokenStream, item: TokenStream) -> TokenStream {
    param_impl::expand(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn find_entity_field(input: &DeriveInput) -> syn::Result<Member> {
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new(
            input.ident.span(),
            "#[signal(entity)] needs a struct",
        ));
    };

    let mut entities = data.fields.iter().enumerate().filter(|(_, field)| {
        matches!(&field.ty, Type::Path(ty)
            if ty.path.segments.last().is_some_and(|segment| segment.ident == "Entity"))
    });

    let found = entities.next();
    match (found, entities.next()) {
        (Some((i, field)), None) => Ok(match &field.ident {
            Some(ident) => Member::Named(ident.clone()),
            None => Member::Unnamed(Index::from(i)),
        }),
        _ => Err(syn::Error::new(
            input.ident.span(),
            "#[signal(entity)] needs exactly one `Entity` field, otherwise name it with `entity = field`",
        )),
    }
}

#[proc_macro_attribute]
pub fn signal(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut entity: Option<Option<Member>> = None;
    let mut input_signal = false;

    let attr_parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("input") {
            input_signal = true;
            Ok(())
        } else if meta.path.is_ident("entity") {
            entity = Some(if meta.input.peek(syn::Token![=]) {
                Some(meta.value()?.parse()?)
            } else {
                None
            });
            Ok(())
        } else {
            Err(meta.error(
                "unsupported #[signal] argument, expected `input`, `entity` or `entity = field`",
            ))
        }
    });
    parse_macro_input!(attr with attr_parser);

    let input = parse_macro_input!(item as DeriveInput);
    let name = generated_name(&input.ident);

    if let Some(error) = reject_data(&input.attrs, "#[signal]")
        .or_else(|| reject_derives(&input.attrs, "#[signal]", TYPE_DERIVES))
    {
        return error;
    }

    if !input.generics.params.is_empty() {
        return syn::Error::new(
            input.generics.span(),
            "#[signal] does not support generic signals",
        )
        .to_compile_error()
        .into();
    }

    let snake_name = format_ident!("{}", name.to_string().to_lowercase());
    let ctor_fn = format_ident!("__register_signal_{}", snake_name);
    let link = linked_crates();
    let (input_impl, input_register) = if input_signal {
        (
            quote! { impl ::reacs::signal::InputSignal for #name {} },
            quote! { ::reacs::record::register_input_signal::<#name>(); },
        )
    } else {
        (quote! {}, quote! {})
    };
    let filter = match entity {
        Some(field) => {
            let field = match field.map_or_else(|| find_entity_field(&input), Ok) {
                Ok(field) => field,
                Err(error) => return error.to_compile_error().into(),
            };
            quote! {
                impl ::reacs::signal::SignalType for #name {
                    type Filter = ::reacs::entity::AliveFilter;
                }

                impl ::reacs::entity::EntitySignal for #name {
                    #[inline]
                    fn entity(&self) -> ::reacs::entity::Entity {
                        self.#field
                    }
                }
            }
        }
        None => quote! {
            impl ::reacs::signal::SignalType for #name {
                type Filter = ::reacs::signal::NoFilter;
            }
        },
    };

    let expanded = quote! {
        #[derive(
            ::core::default::Default,
            ::reacs::hashed::Hashed,
            ::core::clone::Clone,
            ::core::marker::Copy,
            ::reacs::__private::serde::Serialize,
            ::reacs::__private::serde::Deserialize,
        )]
        #[serde(crate = "::reacs::__private::serde")]
        #input

        #filter

        #input_impl

        #[::reacs::__private::ctor::ctor(unsafe, naked, crate_path = ::reacs::__private::ctor)]
        #[allow(non_snake_case)]
        fn #ctor_fn() {
            #link
            type Signals = ::reacs::signal::Signals<#name>;
            type Data = ::reacs::signal::SignalData<#name>;
            ::reacs::registry::register_entry::<Signals>(
                <Signals as ::reacs::hashed::Hashed>::TYPE_UUID,
            );
            ::reacs::registry::register_entry::<Data>(
                <Data as ::reacs::hashed::Hashed>::TYPE_UUID,
            );
            ::reacs::signal::register_signal::<#name>();
            #input_register
        }
    };

    TokenStream::from(expanded)
}
