use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{DeriveInput, FnArg, ItemFn, parse_macro_input, spanned::Spanned};
use uuid::Uuid;

fn generate_uuid_v5(struct_name: &str) -> u128 {
    let engine_namespace = Uuid::parse_str("804277dc-701f-41c1-b670-fbf99628435f").unwrap();
    let crate_name = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
    let qualified = format!("{crate_name}::{struct_name}");

    let uuid = Uuid::new_v5(&engine_namespace, qualified.as_bytes());
    uuid.as_u128()
}

#[proc_macro_derive(Hashed)]
pub fn derive_hashed_def(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let name_str = name.to_string();
    let hash_value = generate_uuid_v5(&name_str);

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let expanded = quote! {
        impl #impl_generics ::reacs::hashed::Hashed for #name #ty_generics #where_clause {
            const TYPE_UUID: u128 = #hash_value;
        }
    };

    TokenStream::from(expanded)
}

#[proc_macro_attribute]
pub fn resource(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    let name = &input.ident;
    let snake_name = format_ident!("{}", name.to_string().to_lowercase());
    let ctor_fn = format_ident!("__register_{}", snake_name);

    let expanded = quote! {
        #[derive(::core::default::Default, ::reacs::hashed::Hashed, ::serde::Serialize, ::serde::Deserialize)]
        #input

        #[::ctor::ctor]
        fn #ctor_fn() {
            ::reacs::registry::register_entry::<#name>(<#name as ::reacs::hashed::Hashed>::TYPE_UUID);
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
