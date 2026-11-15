//! Procedural macros for [reacs](https://crates.io/crates/reacs).
//!
//! Use them through `reacs::prelude`, which re-exports every macro here. A crate using reacs
//! doesn't need this one as a dependency.

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

/// Creates a new `World`, making sure every plugin crate is linked.
///
/// Systems and types register themselves before `main` from every crate linked into the
/// program, but a crate that no code names isn't linked, and its registrations never run. Every
/// reacs macro names all the crates the current crate depends on, so `World::new()` is enough
/// when the app crate uses at least one reacs macro. An app crate that uses none, like a
/// `main.rs` that only builds and steps a world made of plugins, should create its world with
/// `world!()` instead, or name each plugin with `use plugin as _;`. Without either, the systems
/// of plugins the code never names are silently missing.
///
/// In every other respect, `world!()` is the same as `World::new()`.
///
/// # Examples
///
/// ```rust,standalone_crate
/// use reacs::prelude::*;
///
/// fn main() {
///     let mut world = world!();
///     world.step();
/// }
/// ```
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

/// Derives `Hashed`, giving a type a stable 128-bit id.
///
/// The id is a hash of the type's module path and name, so it's the same on every build and
/// machine as long as the type keeps its name and module. Saves, recordings and the type registry
/// identify types by it. For a generic type, the ids of its type parameters and the values of its
/// const parameters are mixed in, so `Foo<A>` and `Foo<B>` get different ids.
///
/// `#[resource]`, `#[component]` and `#[signal]` already derive it, so game code rarely needs this
/// derive. The prelude exports it as `DeriveHashed`, next to the `Hashed` trait.
///
/// Renaming a type or moving it to another module changes its id, so saves made before the change
/// can't load it.
///
/// # Examples
///
/// ```
/// use reacs::prelude::*;
///
/// #[derive(DeriveHashed)]
/// struct Faction;
///
/// #[derive(DeriveHashed)]
/// struct Unit;
///
/// assert_ne!(Faction::TYPE_UUID, Unit::TYPE_UUID);
/// ```
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

/// Declares a resource: a single value shared by the whole world.
///
/// Resources hold state that doesn't belong to one entity: scores, settings, the current wave, or
/// variable-size data keyed by entity, like inventories. Systems read a resource with a
/// `Resource<T>` param and write it with `Mut<Resource<T>>`. A resource starts with its `Default`
/// value in every world that has a system using it, so systems never find it missing.
///
/// The macro derives `Default`, `Hashed`, `Serialize` and `Deserialize`, so don't derive them
/// yourself, and registers the type, so the resource is saved, loaded and hashed with the world.
/// Every field must implement `Default`, `Serialize` and `Deserialize`; declare your own field
/// types, like an enum, with `#[data]` (or `#[data(no_copy)]`). Store maps as `IndexMap`,
/// never `std::collections::HashMap`: a `HashMap`'s iteration order changes after a save and load,
/// which breaks determinism.
///
/// # Local resources
///
/// `#[resource(local)]` declares a resource that belongs to one world and is never saved, hashed or
/// registered. Only `Default` is derived, so it needs no serde support and can hold types like
/// `Rc`. Systems use it through `LocalResource<T>` and `Mut<LocalResource<T>>`.
///
/// Local resources are for caches, and game logic must not depend on them: whatever the
/// simulation reads from a local resource must be rebuildable from saved state. Otherwise loading
/// a save or replaying a recording gives different results.
///
/// `#[resource(local, on_load = method)]` calls `method(&mut self, world: &World)` once after
/// every `World::load` or `World::restore`, to rebuild the cache from the loaded state. Hooks run
/// in an order that can change between builds, so a hook should only read saved resources, not
/// other local ones.
///
/// # Compile errors
///
/// Generic resources aren't supported, and `on_load` requires `local`. Deriving a trait the macro
/// derives, or adding `#[data]`, is an error.
///
/// # Examples
///
/// ```rust,standalone_crate
/// use reacs::prelude::*;
///
/// #[resource]
/// struct Score {
///     points: u64,
/// }
///
/// #[system]
/// fn score_a_point(mut score: Mut<Resource<Score>>) {
///     score.points += 1;
/// }
///
/// fn main() {
///     let mut world = World::new();
///     world.step();
///     world.step();
///     assert_eq!(world.get_resource::<Score>().unwrap().points, 2);
/// }
/// ```
///
/// A local cache, rebuilt after every load:
///
/// ```rust,standalone_crate
/// use reacs::prelude::*;
///
/// #[resource]
/// struct Score {
///     points: u64,
/// }
///
/// #[resource(local, on_load = rebuild)]
/// struct ScoreText {
///     text: String,
/// }
///
/// impl ScoreText {
///     fn rebuild(&mut self, world: &World) {
///         let points = world.get_resource::<Score>().unwrap().points;
///         self.text = format!("{points} points");
///     }
/// }
///
/// fn main() {
///     let mut world = World::build(|setup| setup.insert_resource(Score { points: 3 }));
///     world.init_local_resource::<ScoreText>();
///
///     let save = world.save();
///     world.load(&save);
///     assert_eq!(world.get_local_resource::<ScoreText>().unwrap().text, "3 points");
/// }
/// ```
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

/// Declares a component: a small piece of plain data attached to an entity.
///
/// Systems reach components by `Entity`, usually one named by a signal: `Component<T>` reads them,
/// and `Mut<Component<T>>` also writes, inserts and removes them. Components are never iterated:
/// there's no way to visit every entity that has one.
///
/// Every component type keeps one slot per entity index, which makes access O(1) but costs memory
/// for every index, so keep components small. Variable-size data per entity, like an inventory,
/// belongs in a resource keyed by `Entity`.
///
/// The macro derives `Clone`, `Copy`, `Default`, `Hashed`, `Serialize` and `Deserialize`, so don't
/// derive them yourself, and registers the type, so components are saved and loaded with the
/// world. Every field must implement `Copy`, `Default`, `Serialize` and `Deserialize`; declare
/// your own field types, like an enum, with `#[data]`.
///
/// # Compile errors
///
/// Generic components aren't supported, and neither is deriving a trait the macro derives, or
/// adding `#[data]`.
///
/// # Examples
///
/// ```rust,standalone_crate
/// use reacs::prelude::*;
///
/// #[component]
/// struct Health(i32);
///
/// #[signal(entity)]
/// struct Damage {
///     target: Entity,
///     amount: i32,
/// }
///
/// #[system]
/// fn apply_damage(mut damage: Mut<Receiver<Damage>>, mut health: Mut<Component<Health>>) {
///     for hit in damage.receive() {
///         if let Some(health) = health.get_mut(hit.target) {
///             health.0 -= hit.amount;
///         }
///     }
/// }
///
/// fn main() {
///     let mut ship = Entity::default();
///     let mut world = World::build(|setup| {
///         ship = setup.spawn();
///         setup.set_component(ship, Health(10));
///         setup.signal::<Damage>().send(Damage { target: ship, amount: 3 }, For(2));
///     });
///
///     world.step();
///     world.step();
///     world.step();
///     assert_eq!(world.get_component::<Health>(ship).unwrap().0, 4);
/// }
/// ```
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
/// ```rust,standalone_crate
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
/// ```rust,standalone_crate
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

/// Registers a function as a system.
///
/// A system is a plain function whose arguments are system params: `Resource<T>`, `Component<T>`,
/// `EntityManager`, `LocalResource<T>`, `WorldAccess`, the signal params `Receiver<S>`, `Sender<S>`
/// and `Signal<S>`, or a `#[param]` bundle of those. Wrap a param in `Mut<...>` to write through
/// it. A system takes at most 8 params; group more in a `#[param]` struct.
///
/// Every `#[system]` in every crate linked into the program registers itself before `main`.
/// `World::step` runs them one after another, each finishing before the next starts, in an order
/// fixed when the program starts. Systems are stateless: keep state in resources.
///
/// # Ordering
///
/// - `run_before(a, b)` and `run_after(a, b)` order the system against other systems, named by
///   path, including systems from other crates (`run_after(plugin::apply_damage)`).
/// - `in_group(g)` makes the system a member of the group `g`. A group is an ordinary system,
///   usually with no params. A member runs after `g` and before everything ordered after `g`, so
///   `run_after(g)` means "after the whole group". A group and its members run as one block, so
///   nothing outside the group can run between them.
///
/// Systems with no order between them run in a fixed but unspecified order. Add `run_before` or
/// `run_after` whenever one system must see another's work in the same tick.
/// `World::print_system_order` prints the final order.
///
/// # Waking
///
/// A system with at least one `Mut<Receiver<S>>` param, and no `Mut<Signal<S>>` or other param
/// that forces it to run, sleeps: it's skipped on ticks where none of its receivers has anything
/// new. The whole system is skipped, not only its receive loop, so work that doesn't come from
/// signals, like a timer counted down every tick, doesn't belong in a sleeping system. Systems
/// without a receiver run every tick.
///
/// # Panics
///
/// When the program starts, if the ordering has a cycle, a system is in more than one group, or a
/// group is a member of another group.
///
/// When the system runs, if two of its params borrow the same data and one of them writes it: for
/// example `Resource<Score>` with `Mut<Resource<Score>>`, or `Mut<Receiver<S>>` with
/// `Mut<Sender<S>>` for the same `S` (use `Mut<Signal<S>>` to do both).
///
/// # Compile errors
///
/// Methods taking `self`, and arguments that aren't system params, including a signal param
/// without `Mut`.
///
/// # Examples
///
/// ```rust,standalone_crate
/// use reacs::prelude::*;
///
/// #[resource]
/// struct Log {
///     lines: Vec<String>,
/// }
///
/// #[system]
/// fn combat() {}
///
/// #[system(in_group(combat))]
/// fn shoot(mut log: Mut<Resource<Log>>) {
///     log.lines.push("shoot".into());
/// }
///
/// #[system(in_group(combat), run_after(shoot))]
/// fn hit(mut log: Mut<Resource<Log>>) {
///     log.lines.push("hit".into());
/// }
///
/// #[system(run_after(combat))]
/// fn report(mut log: Mut<Resource<Log>>) {
///     log.lines.push("report".into());
/// }
///
/// fn main() {
///     let mut world = World::new();
///     world.step();
///     assert_eq!(world.get_resource::<Log>().unwrap().lines, ["shoot", "hit", "report"]);
/// }
/// ```
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

/// Bundles system params into one struct, used as a single param.
///
/// Write each field's type without a lifetime or mode, like `Component<Health>` or
/// `Resource<Log>`; the macro adds both. The bundle is then a param like any other: `Combat` reads
/// through every field, and `Mut<Combat>` writes through them. Bundles share a set of params
/// between systems and get past the limit of 8 params per system.
///
/// # Modes
///
/// - `#[param]`: usable as `Combat` and as `Mut<Combat>`.
/// - `#[param(read)]`: only as `Combat`.
/// - `#[param(write)]`: only as `Mut<Combat>`. Needed when a field only exists for writing, like a
///   `Receiver<S>`.
///
/// Mark a field `#[read]` or `#[write]` to keep that mode whatever the bundle's mode, like a
/// `#[read] Resource<Rules>` in a bundle that is otherwise written. At least one field must stay
/// unmarked. A bundle with a single field dereferences to that field's target.
///
/// # Waking
///
/// A bundle wakes its system like its fields would: one holding a `Receiver<S>` lets its system
/// sleep when there's nothing to receive, and one holding a `Signal<S>` makes it run every tick
/// (see `#[system]`). `#[param(wake = always)]` makes the system run every tick anyway, and combines
/// with a mode: `#[param(write, wake = always)]`.
///
/// # Generics
///
/// Type and const parameters are allowed, and type parameters get a `'static` bound. `M` is
/// reserved for the mode. Lifetimes aren't allowed: the macro adds `'w` itself.
///
/// # Compile errors
///
/// A field that isn't a system param, a struct with no fields, a struct whose fields are all
/// marked `#[read]` or `#[write]`, a lifetime parameter, or a type parameter named `M`.
///
/// # Examples
///
/// ```rust,standalone_crate
/// use reacs::prelude::*;
///
/// #[component]
/// struct Health(i32);
///
/// #[resource]
/// struct Log {
///     lines: Vec<String>,
/// }
///
/// #[param]
/// struct Combat {
///     health: Component<Health>,
///     log: Resource<Log>,
///     tick: Tick,
/// }
///
/// #[signal(entity)]
/// struct Damage {
///     target: Entity,
///     amount: i32,
/// }
///
/// #[system]
/// fn apply_damage(mut damage: Mut<Receiver<Damage>>, mut combat: Mut<Combat>) {
///     let tick = combat.tick.get();
///     for hit in damage.receive() {
///         if let Some(health) = combat.health.get_mut(hit.target) {
///             health.0 -= hit.amount;
///             combat.log.lines.push(format!("tick {tick}: {} damage", hit.amount));
///         }
///     }
/// }
///
/// fn main() {
///     let mut ship = Entity::default();
///     let mut world = World::build(|setup| {
///         ship = setup.spawn();
///         setup.set_component(ship, Health(10));
///         setup.signal::<Damage>().emit(Damage { target: ship, amount: 3 });
///     });
///
///     world.step();
///     assert_eq!(world.get_component::<Health>(ship).unwrap().0, 7);
///     assert_eq!(world.get_resource::<Log>().unwrap().lines, ["tick 0: 3 damage"]);
/// }
/// ```
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

/// Declares a signal: a message systems send to each other.
///
/// Signals drive everything in reacs, and they're the only thing it iterates: a system runs
/// because of the signals it receives, and touches the entities they name. A signal lasts one tick
/// or a window of ticks (see `IntoWindow`). Systems use a signal type through one of three params:
///
/// - `Mut<Receiver<S>>` receives. Its system sleeps on ticks with nothing to receive (see
///   `#[system]`).
/// - `Mut<Sender<S>>` sends, modifies and cancels.
/// - `Mut<Signal<S>>` does both, and its system runs every tick.
///
/// A signal reaches every system that receives its type: in the same tick if the receiver runs
/// after the sender, otherwise in the next tick. Signals are queued, not callbacks: each receiver
/// handles them at its own place in the system order.
///
/// The macro derives `Clone`, `Copy`, `Default`, `Hashed`, `Serialize` and `Deserialize`, so don't
/// derive them yourself, and registers the type. Keep payloads small and plain. Every field must
/// implement `Copy`, `Default`, `Serialize` and `Deserialize`; declare your own field types, like an
/// enum, with `#[data]`.
///
/// # Entity signals
///
/// `#[signal(entity)]` ties each signal to the life of an entity: once the entity is despawned, its
/// signals are never delivered again, and open-ended ones are dropped from memory. The macro uses
/// the struct's only `Entity` field. With several, or with a type alias for `Entity`, name the
/// field: `entity = target`, or `entity = 1` for a tuple struct. Use entity signals for work on a
/// living entity, like `Burn` or `Move`.
///
/// News about an entity must stay a plain `#[signal]`. A `#[signal(entity)] struct Died(Entity)`
/// never arrives, because its entity is already dead when it's sent, and nothing reports it.
/// Receivers of a plain signal check `EntityManager::is_alive` when it matters.
///
/// # Input signals
///
/// `#[signal(input)]` lets the host send the signal with `World::input`, the only way input gets
/// into a running world. Every input signal exists in every world, so sending one that no system
/// uses is fine. Systems use input signals like any other. `input` combines with `entity`:
/// `#[signal(input, entity)]`.
///
/// # Compile errors
///
/// Generic signals aren't supported, and neither is deriving a trait the macro derives, or adding
/// `#[data]`. `entity` needs a struct with exactly one `Entity` field, or the field named with
/// `entity = field`.
///
/// # Examples
///
/// ```rust,standalone_crate
/// use reacs::prelude::*;
///
/// #[signal(input)]
/// struct Clicked {
///     x: i32,
///     y: i32,
/// }
///
/// #[resource]
/// struct Clicks {
///     last: (i32, i32),
///     count: u32,
/// }
///
/// #[system]
/// fn count_clicks(mut clicks: Mut<Receiver<Clicked>>, mut total: Mut<Resource<Clicks>>) {
///     for click in clicks.receive() {
///         total.last = (click.x, click.y);
///         total.count += 1;
///     }
/// }
///
/// fn main() {
///     let mut world = World::new();
///     world.input(Clicked { x: 3, y: 4 });
///     world.input(Clicked { x: 5, y: 1 });
///     world.step();
///
///     let clicks = world.get_resource::<Clicks>().unwrap();
///     assert_eq!((clicks.count, clicks.last), (2, (5, 1)));
/// }
/// ```
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
