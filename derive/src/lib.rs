use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{
    DeriveInput, FnArg, GenericParam, ItemFn, Path, parse_macro_input, parse_quote,
    spanned::Spanned,
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
    let name = &input.ident;
    let snake_name = format_ident!("{}", name.to_string().to_lowercase());
    let ctor_fn = format_ident!("__register_type_{}", snake_name);
    let link = linked_crates();

    if local {
        if !input.generics.params.is_empty() {
            return syn::Error::new(
                input.generics.span(),
                "#[resource(local)] does not support generic resources",
            )
            .to_compile_error()
            .into();
        }

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

        #[::reacs::__private::ctor::ctor]
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
    let name = &input.ident;
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

        #[::reacs::__private::ctor::ctor]
        #[allow(non_snake_case)]
        fn #ctor_fn() {
        #link
        type Map = ::reacs::component::ComponentMap<#name>;
        ::reacs::registry::register_entry::<Map>(
            <Map as ::reacs::hashed::Hashed>::TYPE_UUID,
            );
        ::reacs::component::register_component::<#name>();
        }
    };

    TokenStream::from(expanded)
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

        #[::reacs::__private::ctor::ctor]
        #[allow(non_snake_case)]
        fn #ctor_fn() {
            #link
            #( let _ = #run_before; )*
            #( let _ = #run_after; )*
            #( let _ = #in_group; )*
            let f: for<'w> fn(#(<#param_types as ::reacs::schedule::SystemArg>::Item<'w>),*) = #name;
            ::reacs::schedule::register_system(
                ::reacs::schedule::IntoSystem::into_system(f)
                    .named(::core::module_path!(), #name_str)
                    .run_before(&[#(<#run_before as ::reacs::schedule::SystemId>::UUID),*])
                    .run_after(&[#(<#run_after as ::reacs::schedule::SystemId>::UUID),*])
                    .in_group(&[#(<#in_group as ::reacs::schedule::SystemId>::UUID),*])
            );
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

#[proc_macro_attribute]
pub fn signal(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    let name = &input.ident;

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

        impl ::reacs::signal::SignalType for #name {}

        #[::reacs::__private::ctor::ctor]
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
        }
    };

    TokenStream::from(expanded)
}
