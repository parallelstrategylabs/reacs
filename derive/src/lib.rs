use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{
    DeriveInput, FnArg, GenericParam, ItemFn, parse_macro_input, parse_quote, spanned::Spanned,
};

mod param_impl;

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
pub fn resource(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    let name = &input.ident;
    let snake_name = format_ident!("{}", name.to_string().to_lowercase());
    let ctor_fn = format_ident!("__register_type_{}", snake_name);

    let expanded = quote! {
        #[derive(
            ::core::default::Default,
            ::reacs::hashed::Hashed,
            ::serde::Serialize,
            ::serde::Deserialize,
        )]
        #input

        #[::ctor::ctor]
        fn #ctor_fn() {
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

    let expanded = quote! {
        #[derive(
            ::core::default::Default,
            ::reacs::hashed::Hashed,
            ::core::clone::Clone,
            ::core::marker::Copy,
            ::serde::Serialize,
            ::serde::Deserialize,
        )]
        #input

        #[::ctor::ctor]
        #[allow(non_snake_case)]
        fn #ctor_fn() {
        type Map = ::reacs::direct_map::DirectMap<#name>;
        ::reacs::registry::register_entry::<Map>(
            <Map as ::reacs::hashed::Hashed>::TYPE_UUID,
            );
        }
    };

    TokenStream::from(expanded)
}

#[proc_macro_attribute]
pub fn system(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let name = &input.sig.ident;
    let ctor_fn = format_ident!("__register_system_{}", name);

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

        #[::ctor::ctor]
        #[allow(non_snake_case)]
        fn #ctor_fn() {
            let f: for<'w> fn(#(<#param_types as ::reacs::schedule::SystemParam>::Item<'w>),*) = #name;
            ::reacs::schedule::register_system(
                ::reacs::schedule::IntoSystem::into_system(f)
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